//! Background-process integration tests.
//!
//! Exercises the full `bash_bg` flow end-to-end via the public
//! `StateManager` surface: spawn → reader captures output → drain
//! assembles synthetic turn. Full agent-loop wiring
//! (`QueueMessage::BackgroundOutputReady` → drain seam) is exercised by
//! the unit tests in `bg_processes.rs::tests` plus the wire-shape
//! coverage here.
//!
//! These tests spawn real `sh` processes, so they require a working
//! `sh` on `$PATH`. They're tagged `tokio::test` because they wait
//! for short async sleeps to let the reader thread drain the PTY.

use peakbot::bg_processes::{DEFAULT_CAPTURE_LINES, StartParams};
use peakbot::state::StateManager;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Helper: build a StateManager with the bg notify bridge attached and
/// return both the manager and the bridge receiver so tests can poll
/// for pings without going through the full agent loop.
fn make_sm_with_bridge() -> (Arc<StateManager>, mpsc::UnboundedReceiver<()>) {
    let sm = Arc::new(StateManager::new());
    let (tx, rx) = mpsc::unbounded_channel::<()>();
    sm.attach_bg_notify(tx);
    (sm, rx)
}

/// Poll up to `timeout` for the bg buffer of process `id` to contain at
/// least one line. Returns true on success, false on timeout.
async fn wait_for_buffered_line(sm: &StateManager, id: u32, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        let rows = sm.list_bg();
        if let Some(r) = rows.iter().find(|r| r.id == id)
            && r.buffer_len > 0
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Best-effort process-group cleanup so a RED run never leaks the
/// HUP-immune heartbeat loop. The per-run tempdir path is unique, so the
/// marker can't hit anything else.
#[cfg(unix)]
struct PkillGuard(String);

#[cfg(unix)]
impl Drop for PkillGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("pkill")
            .args(["-f", &self.0])
            .output();
    }
}

/// Read the heartbeat file, retrying past the brief window where the
/// writer has truncated it but not yet written the new value.
#[cfg(unix)]
async fn read_beat_file(path: &std::path::Path) -> Option<String> {
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(300) {
        if let Ok(s) = std::fs::read_to_string(path)
            && !s.trim().is_empty()
        {
            return Some(s.trim().to_string());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

#[tokio::test]
async fn bg_start_captures_echoed_lines_into_synthetic_turn() {
    let (sm, _rx) = make_sm_with_bridge();
    let entry = sm
        .start_bg(StartParams {
            command: "echo hi from bg; sleep 0.05".into(),
            capture_cap: DEFAULT_CAPTURE_LINES,
            cwd: None,
            label: None,
            cooldown: Duration::ZERO,
            env: None,
            shell: String::new(),
        })
        .expect("start_bg should succeed");
    assert!(entry.id >= 1, "id should be monotonic from 1");

    // Wait for the reader to drain at least one line + the EOF flush.
    let got = wait_for_buffered_line(&sm, entry.id, Duration::from_secs(3)).await;
    assert!(got, "reader should have captured at least the echoed line");

    let synth = sm
        .drain_bg_output_into_synthetic_turn()
        .expect("drain should return a synthetic turn");
    assert!(
        synth.text.starts_with("[bg output]"),
        "synthetic turn must carry the [bg output] sentinel; got: {}",
        synth.text
    );
    assert!(
        synth.text.contains("hi from bg"),
        "captured line must appear in the synthetic turn; got: {}",
        synth.text
    );
    assert_eq!(synth.proc_ids, vec![entry.id]);
}

#[tokio::test]
async fn bg_zero_cooldown_drains_in_real_time() {
    let (sm, _rx) = make_sm_with_bridge();
    let entry = sm
        .start_bg(StartParams {
            command: "echo from-telegram".into(),
            capture_cap: 50,
            cwd: None,
            label: Some("telegram".into()),
            cooldown: Duration::ZERO,
            env: None,
            shell: String::new(),
        })
        .expect("start_bg should succeed");

    let got = wait_for_buffered_line(&sm, entry.id, Duration::from_secs(3)).await;
    assert!(got);

    let synth = sm.drain_bg_output_into_synthetic_turn().expect("drain");
    assert!(
        synth.text.contains("from-telegram"),
        "zero-cooldown output must drain immediately; got: {}",
        synth.text
    );
}

#[tokio::test]
async fn bg_stop_returns_exit_code_and_final_lines() {
    let (sm, _rx) = make_sm_with_bridge();
    // Use a process that runs long enough to be stopped — but exits
    // promptly on SIGHUP so the test doesn't drag.
    let entry = sm
        .start_bg(StartParams {
            command: "echo first line; sleep 30".into(),
            capture_cap: 10,
            cwd: None,
            label: None,
            cooldown: Duration::ZERO,
            env: None,
            shell: String::new(),
        })
        .expect("start_bg should succeed");

    let _ = wait_for_buffered_line(&sm, entry.id, Duration::from_secs(2)).await;

    let (exit_code, final_lines) = sm.stop_bg(entry.id).expect("stop_bg should succeed");
    // exit_code is best-effort here; we just need stop to be quick and
    // the final tail to carry the line we observed.
    let _ = exit_code;
    let buffer_blob = final_lines.join("\n");
    assert!(
        buffer_blob.contains("first line"),
        "stop should return the captured tail; got: {buffer_blob}"
    );

    // After stop the process is gone from list.
    let rows = sm.list_bg();
    assert!(
        !rows.iter().any(|r| r.id == entry.id),
        "stop must remove the process from the registry"
    );
}

#[tokio::test]
async fn bg_list_reflects_running_state() {
    let (sm, _rx) = make_sm_with_bridge();
    let entry = sm
        .start_bg(StartParams {
            command: "sleep 30".into(),
            capture_cap: 0,
            cwd: None,
            label: Some("sleeper".into()),
            cooldown: Duration::ZERO,
            env: None,
            shell: String::new(),
        })
        .expect("start_bg");

    let rows = sm.list_bg();
    let row = rows
        .iter()
        .find(|r| r.id == entry.id)
        .expect("started process must appear in list");
    assert_eq!(row.label.as_deref(), Some("sleeper"));
    assert_eq!(row.cooldown, Duration::ZERO);

    // Cleanup.
    let _ = sm.stop_bg(entry.id);
}

#[tokio::test]
async fn bg_clear_kills_all_processes() {
    let (sm, _rx) = make_sm_with_bridge();
    let a = sm
        .start_bg(StartParams {
            command: "sleep 30".into(),
            capture_cap: 0,
            cwd: None,
            label: None,
            cooldown: Duration::ZERO,
            env: None,
            shell: String::new(),
        })
        .expect("start a")
        .id;
    let b = sm
        .start_bg(StartParams {
            command: "sleep 30".into(),
            capture_cap: 0,
            cwd: None,
            label: None,
            cooldown: Duration::ZERO,
            env: None,
            shell: String::new(),
        })
        .expect("start b")
        .id;
    assert_eq!(sm.list_bg().len(), 2);

    sm.clear_bg();

    let rows = sm.list_bg();
    assert!(rows.is_empty(), "clear_bg should drop every process");
    let _ = (a, b);
}

#[tokio::test]
async fn bg_drain_appends_synthetic_user_message_with_background_source() {
    let (sm, _rx) = make_sm_with_bridge();
    let entry = sm
        .start_bg(StartParams {
            command: "echo greet world".into(),
            capture_cap: 10,
            cwd: None,
            label: None,
            cooldown: Duration::ZERO,
            env: None,
            shell: String::new(),
        })
        .expect("start_bg");
    let got = wait_for_buffered_line(&sm, entry.id, Duration::from_secs(3)).await;
    assert!(got);

    let synth = sm
        .drain_bg_output_into_synthetic_turn()
        .expect("drain returned a turn");
    sm.add_user_message_from_background(synth.text.clone(), synth.proc_ids.clone());

    let state = sm.get_state();
    let last = state
        .chat
        .messages
        .last()
        .expect("a synthetic user message landed");
    // The discriminator must be Background (not Human).
    use peakbot::ui::app_state::MessageSource;
    match &last.source {
        MessageSource::Background { proc_ids } => {
            assert_eq!(proc_ids, &vec![entry.id]);
        }
        other => panic!("synthetic turn must carry Background source, got {other:?}"),
    }
}

/// `stop_bg` must kill the whole process group: a grandchild that ignores
/// SIGHUP keeps the PTY slave open and the reader thread wedged. The
/// heartbeat file is rewritten every 100 ms by the HUP-immune grandchild;
/// a live one keeps growing after `stop_bg`.
#[cfg(unix)]
#[tokio::test]
async fn bg_stop_kills_hup_immune_grandchild() {
    let (sm, _rx) = make_sm_with_bridge();
    let dir = tempfile::tempdir().expect("create temp dir");
    let beat = dir.path().join("beat");
    let command = format!(
        "(trap '' HUP; i=0; while :; do i=$((i+1)); echo $i > '{}/beat'; sleep 0.1; done) & echo ready; wait",
        dir.path().display()
    );
    let _guard = PkillGuard(dir.path().display().to_string());

    let entry = sm
        .start_bg(StartParams {
            command,
            capture_cap: DEFAULT_CAPTURE_LINES,
            cwd: None,
            label: Some("pg-kill".into()),
            cooldown: Duration::ZERO,
            env: None,
            shell: String::new(),
        })
        .expect("start_bg should succeed");

    // Liveness probe: the heartbeat exists only once the HUP-immune
    // grandchild is actually running.
    let started = std::time::Instant::now();
    while !beat.exists() {
        if started.elapsed() > Duration::from_secs(3) {
            panic!("heartbeat file never appeared — grandchild never started");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let _ = sm.stop_bg(entry.id).expect("stop_bg should succeed");

    // Two samples 1 s apart: a live grandchild rewrites the file every
    // 100 ms, so the values must differ; a dead one freezes them.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let v1 = read_beat_file(&beat).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let v2 = read_beat_file(&beat).await;
    assert_eq!(
        v1, v2,
        "the HUP-immune grandchild must be dead after stop_bg — the \
         heartbeat kept growing (v1={v1:?}, v2={v2:?})"
    );
}
