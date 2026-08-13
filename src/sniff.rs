//! Wire-truth debug capture — the `PEAKBOT_SNIFF` JSONL log.
//!
//! Armed at boot with `PEAKBOT_SNIFF=<path>` (the value *is* the path), off
//! otherwise. Every LLM call writes two lines: a `req` before the call and a
//! `res` after, paired by a process-monotonic `id`. Capture happens at the
//! `SessionHook` seam, so records are labelled `"kind":"logical"` — they are
//! what PeakBot handed rig and what rig handed back, not HTTP bytes.
//!
//! Full rationale, schema and anti-goals: `docs/http-sniffer-design.md`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rig_core::completion::message::{AssistantContent, Message};
use rig_core::one_or_many::OneOrMany;
use serde::Serialize;
use serde_json::{Value, json};

/// Per-string-leaf cap, in **chars** (design §5).
const MAX_STR: usize = 16_384;

/// Provider + model names for a record. The hook is generic over the model
/// *type*, which carries no name — the pair is only knowable where the agent
/// is built, so it is passed in. One `Option` of a pair because the two are
/// never meaningfully apart.
#[derive(Clone, Debug)]
pub struct WireLabel {
    /// Resolved provider name, e.g. `"anthropic"`.
    pub provider: String,
    /// Model id as configured.
    pub model: String,
}

/// The open sink plus its one-shot warn latch.
struct Sink {
    file: File,
    /// A full disk must not turn into a log flood: warn on the first write
    /// failure only, then keep trying silently.
    warned: bool,
}

/// `None` = disabled. Re-armable: `init` replaces the sink, `init_from_env`
/// with no path clears it. Boot-only in production; the tests need both edges.
static SINK: Mutex<Option<Sink>> = Mutex::new(None);
/// Mirrors `SINK.is_some()` so the off path costs one atomic load, not a lock.
static ENABLED: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Read `PEAKBOT_SNIFF` and arm the sniffer with its value as the path.
/// Unset or empty leaves it disabled. Called once from `main`.
pub fn init_from_env() {
    match std::env::var("PEAKBOT_SNIFF") {
        Ok(path) if !path.is_empty() => init(Path::new(&path)),
        _ => set_sink(None),
    }
}

/// Arm the sniffer on `path` (append, `0600` on unix). An unopenable path
/// warns once and stays disabled — a debug tool must never kill the agent.
pub fn init(path: &Path) {
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        // The file is the whole conversation: no wider than the owner.
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    match opts.open(path) {
        Ok(file) => {
            set_sink(Some(Sink {
                file,
                warned: false,
            }));
            tracing::info!(path = %path.display(), "PEAKBOT_SNIFF armed (logical LLM call capture)");
        }
        Err(e) => {
            set_sink(None);
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "PEAKBOT_SNIFF: could not open sniff file; sniffing stays off"
            );
        }
    }
}

/// Is the sniffer armed? One relaxed atomic load — the guard on every seam.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Next process-monotonic pairing id. Shared across lanes: lane labels are not
/// unique (two concurrent `junior` sub-agents share one), so only this id can
/// pair a `req` with its `res`.
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// The `req` record: what the hook saw us about to send.
pub fn request_record(
    id: u64,
    lane: &str,
    wire: Option<WireLabel>,
    prompt: &Message,
    history: &[Message],
) -> Value {
    record(
        id,
        "req",
        lane,
        wire,
        json!({ "prompt": jsonify(prompt), "history": jsonify(history) }),
    )
}

/// The `res` record: the provider-native `raw` **and** rig's normalised
/// `choice`, side by side. The duplication is the point — a `thinking` block
/// present in `raw` and absent from `choice` localises the loss to rig's
/// mapping in one glance (design §1).
pub fn response_record<R: Serialize>(
    id: u64,
    lane: &str,
    wire: Option<WireLabel>,
    raw: &R,
    choice: &OneOrMany<AssistantContent>,
    usage: &Value,
) -> Value {
    record(
        id,
        "res",
        lane,
        wire,
        json!({ "raw": jsonify(raw), "choice": jsonify(choice), "usage": usage.clone() }),
    )
}

/// Cap every string leaf at `max_chars`, recursively. Base64 images and
/// JSON-in-a-string tool arguments are string leaves like any other — one
/// rule beats five special cases (design §5).
pub fn truncate_in_place(v: &mut Value, max_chars: usize) {
    match v {
        Value::String(s) => {
            if let Some(cut) = truncated(s, max_chars) {
                *s = cut;
            }
        }
        Value::Array(items) => {
            for item in items {
                truncate_in_place(item, max_chars);
            }
        }
        Value::Object(map) => {
            for (_, val) in map.iter_mut() {
                truncate_in_place(val, max_chars);
            }
        }
        _ => {}
    }
}

/// Truncate, serialize and append one line. No-op when disabled; never panics.
pub fn write_record(v: &Value) {
    if !enabled() {
        return;
    }

    let mut v = v.clone();
    truncate_in_place(&mut v, MAX_STR);
    let mut line = match serde_json::to_string(&v) {
        Ok(line) => line,
        Err(e) => {
            tracing::warn!(error = %e, "PEAKBOT_SNIFF: record failed to serialize; dropped");
            return;
        }
    };
    line.push('\n');

    let mut guard = lock_sink();
    let Some(sink) = guard.as_mut() else {
        return;
    };
    // Unbuffered on purpose: the `req` line must be on disk *before* the
    // network call, and `tail -f` must see it (design §6).
    if let Err(e) = sink.file.write_all(line.as_bytes())
        && !sink.warned
    {
        sink.warned = true;
        tracing::warn!(error = %e, "PEAKBOT_SNIFF: write failed; further failures are silent");
    }
}

/// Common envelope for both directions (design §3).
fn record(id: u64, dir: &str, lane: &str, wire: Option<WireLabel>, payload: Value) -> Value {
    let (provider, model) = match wire {
        Some(w) => (Value::String(w.provider), Value::String(w.model)),
        None => (Value::Null, Value::Null),
    };
    json!({
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "id": id,
        "dir": dir,
        "lane": lane,
        "provider": provider,
        "model": model,
        // Constant. Reserved so a future raw-socket capture can say "wire"
        // without any reader having to guess which one it is looking at.
        "kind": "logical",
        "payload": payload,
    })
}

/// A value that cannot be serialized is a hole in the log, not a lost record.
fn jsonify<T: Serialize + ?Sized>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_else(|e| json!({ "peakbot-sniff-error": e.to_string() }))
}

/// `Some(replacement)` when `s` is over the cap. Walks `char_indices`, never
/// byte offsets: a split codepoint would poison the whole JSONL line.
fn truncated(s: &str, max_chars: usize) -> Option<String> {
    let original = s.chars().count();
    if original <= max_chars {
        return None;
    }
    let cut = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    Some(format!(
        "{}…[peakbot-sniff: truncated, kept {max_chars} of {original} chars]",
        &s[..cut]
    ))
}

fn set_sink(sink: Option<Sink>) {
    ENABLED.store(sink.is_some(), Ordering::Relaxed);
    *lock_sink() = sink;
}

/// A poisoned mutex must not take the agent down with it — a debug logger is
/// the last thing allowed to panic.
fn lock_sink() -> std::sync::MutexGuard<'static, Option<Sink>> {
    SINK.lock().unwrap_or_else(|e| e.into_inner())
}
