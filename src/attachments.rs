//! File attachments — immutable stored files referenced by id.
//!
//! An [`Attachment`] is a reference to a file at `<root>/<convo>/<id>/<name>`;
//! the bytes are read at the point of use (turn building, HTTP serving), never
//! carried in messages. Only [`UploadStore`] constructs one (via `describe`),
//! so upload and lookup cannot drift. This module is the only code that knows
//! the on-disk layout.

use crate::config::UploadsConfig;
use crate::vision::MAX_IMAGE_BYTES;
use futures::StreamExt;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::io::StreamReader;
use uuid::Uuid;

/// A file the user attached to a message. Immutable; bytes live at
/// `UploadStore::path(&self)`. Only `UploadStore` constructs one (via `describe`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Attachment {
    /// Upload id = the on-disk dir name.
    pub id: Uuid,
    /// Conversation dir the bytes live in — a location, not an ownership
    /// claim: after a `/new` send race the owning conversation may differ.
    pub convo: Uuid,
    /// Sanitized; IS the on-disk leaf name.
    pub name: String,
    /// Sniffed for images, else `mime_guess` on the name, else octet-stream.
    pub mime: String,
    pub size: u64,
    pub kind: AttachmentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    /// Magic bytes are PNG/JPEG/GIF/WEBP AND size <= `vision::MAX_IMAGE_BYTES`.
    /// The only kind a vision model receives as image content.
    Image,
    /// Everything else (incl. HEIC, SVG, an 11 MB PNG). Delivered as a path
    /// reference in the model note.
    File,
}

/// Per-conversation upload store. The root is created lazily on the first
/// store, so a session that never attaches touches no disk.
#[derive(Clone)]
pub struct UploadStore {
    root: PathBuf,
    limits: UploadsConfig,
}

impl UploadStore {
    pub fn new(root: PathBuf, limits: UploadsConfig) -> Self {
        Self { root, limits }
    }

    /// `<data_local_dir>/peakbot/uploads` — the data dir, not the cache dir,
    /// because OS cache cleaners would silently break history.
    pub fn default_root() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("peakbot")
            .join("uploads")
    }

    /// Boot value — the single runtime source of truth for the limits.
    pub fn limits(&self) -> UploadsConfig {
        self.limits
    }

    pub fn path(&self, a: &Attachment) -> PathBuf {
        self.root
            .join(a.convo.to_string())
            .join(a.id.to_string())
            .join(&a.name)
    }

    /// Stores one file. `name` is sanitized here. `take(max+1)` is the only
    /// size limit in the system — it also bounds unbounded sources like
    /// `/dev/zero`.
    pub async fn store(
        &self,
        convo: Uuid,
        name: &str,
        reader: impl AsyncRead + Unpin,
    ) -> Result<Attachment, AttachError> {
        let name = sanitize_name(name);
        let id = Uuid::new_v4();
        let id_dir = self.root.join(convo.to_string()).join(id.to_string());
        tokio::fs::create_dir_all(&id_dir).await?;
        let file_path = id_dir.join(&name);
        let max_bytes = self.limits.max_file_mb as u64 * 1024 * 1024;

        let mut file = tokio::fs::File::create(&file_path).await?;
        let mut reader = reader.take(max_bytes + 1);
        let written = tokio::io::copy(&mut reader, &mut file).await?;
        if written > max_bytes {
            // One id dir per file, so removing the dir removes the partial.
            let _ = tokio::fs::remove_dir_all(&id_dir).await;
            return Err(AttachError::TooLarge {
                name,
                size: None,
                max_mb: self.limits.max_file_mb,
            });
        }

        Ok(describe(&file_path, convo, id)?)
    }

    /// The single regular file in `root/convo/id`, described. `None` if the
    /// dir is absent or malformed (zero or two+ entries, a non-file).
    pub fn find(&self, convo: Uuid, id: Uuid) -> Option<Attachment> {
        let id_dir = self.root.join(convo.to_string()).join(id.to_string());
        let mut files = Vec::new();
        for entry in std::fs::read_dir(&id_dir).ok()? {
            let entry = entry.ok()?;
            files.push(entry);
        }
        let [entry] = files.as_slice() else {
            return None;
        };
        if !entry.file_type().ok()?.is_file() {
            return None;
        }
        describe(&entry.path(), convo, id).ok()
    }

    /// Best-effort removal of a conversation's upload dir; logs on error.
    pub fn remove_conversation(&self, convo: Uuid) {
        let dir = self.root.join(convo.to_string());
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            // Never uploaded to — nothing to remove, not worth a log line.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(convo = %convo, error = %e, "failed to remove upload dir");
            }
        }
    }
}

/// stat + 16-byte magic sniff → the attachment's metadata. The single
/// constructor for `Attachment` (I2).
fn describe(path: &Path, convo: Uuid, id: Uuid) -> std::io::Result<Attachment> {
    let metadata = std::fs::metadata(path)?;
    let size = metadata.len();
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let mut sniff = [0u8; 16];
    let mut n = 0;
    if size > 0 {
        let mut f = std::fs::File::open(path)?;
        n = std::io::Read::read(&mut f, &mut sniff)?;
    }

    // `guess_format` is a static magic table — no codec features needed.
    let sniffed = image::guess_format(&sniff[..n]).ok();
    let (kind, mime) = match sniffed {
        Some(fmt)
            if matches!(
                fmt,
                image::ImageFormat::Png
                    | image::ImageFormat::Jpeg
                    | image::ImageFormat::Gif
                    | image::ImageFormat::WebP
            ) && size <= MAX_IMAGE_BYTES as u64 =>
        {
            (AttachmentKind::Image, sniffed_mime(fmt).to_string())
        }
        // A sniffed-but-oversized image is a File whose mime comes from the
        // extension, like every other non-image.
        _ => (
            AttachmentKind::File,
            mime_guess::from_path(&name)
                .first_or_octet_stream()
                .to_string(),
        ),
    };

    Ok(Attachment {
        id,
        convo,
        name,
        mime,
        size,
        kind,
    })
}

fn sniffed_mime(format: image::ImageFormat) -> &'static str {
    match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        _ => unreachable!("only called for the four sniffable formats"),
    }
}

/// The fixed model-facing note. Empty string when `atts` is empty. A pure
/// function of the attachments, so it is stable across turns (prompt-cache
/// friendly).
pub fn model_note(store: &UploadStore, atts: &[Attachment]) -> String {
    if atts.is_empty() {
        return String::new();
    }
    let mut lines = Vec::with_capacity(atts.len());
    for a in atts {
        lines.push(format!(
            "- {} ({}, {}): {}",
            a.name,
            a.mime,
            human_size(a.size),
            store.path(a).display()
        ));
    }
    format!(
        "[Attached files — read them with your file tools]\n{}",
        lines.join("\n")
    )
}

/// "512 B" / "1.2 KB" / "2.3 MB" — base 1024, one decimal.
pub fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    if bytes < KB {
        return format!("{bytes} B");
    }
    let units = ["KB", "MB", "GB", "TB", "PB"];
    // `value` is in `units[unit]` units: start in KB, divide up as needed.
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < units.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", units[unit])
}

/// `[img:TOKEN]` grammar only, no I/O. Returns stripped text + raw tokens in
/// order. An unterminated `[img:` stays literal (today's behaviour).
/// `data:` tokens are a grammar error; `http(s)://` URLs are normal tokens
/// (amendment A1 — `collect` downloads them into the store).
pub fn parse_image_tokens(buf: &str) -> Result<(String, Vec<String>), AttachError> {
    const MARKER: &str = "[img:";
    let mut out_text = String::with_capacity(buf.len());
    let mut tokens = Vec::new();
    let mut cursor = 0;

    while let Some(rel_start) = buf[cursor..].find(MARKER) {
        let start = cursor + rel_start;
        let token_start = start + MARKER.len();
        let Some(rel_end) = buf[token_start..].find(']') else {
            // No closing bracket — leave the rest of the buffer literal.
            break;
        };
        let token_end = token_start + rel_end;
        let token = buf[token_start..token_end].trim();

        out_text.push_str(&buf[cursor..start]);
        validate_token(token)?;
        tokens.push(token.to_string());
        cursor = token_end + 1;
    }
    out_text.push_str(&buf[cursor..]);
    Ok((out_text, tokens))
}

fn validate_token(token: &str) -> Result<(), AttachError> {
    if token.starts_with("data:") {
        return Err(AttachError::DataUriToken);
    }
    if let Some(scheme) = token.split_once("://").map(|(s, _)| s) {
        // Only http(s) URLs are kept (A1); any other scheme is not a path.
        if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
            return Ok(());
        }
        return Err(AttachError::InvalidToken(token.to_string()));
    }
    // A path: absolute, `~/`, `./`, or containing a separator. A bare word
    // is neither a path nor a URL.
    if token.starts_with('/')
        || token.starts_with('~')
        || token.starts_with("./")
        || token.contains('/')
        || token.contains('\\')
    {
        return Ok(());
    }
    Err(AttachError::InvalidToken(token.to_string()))
}

/// Controller ingest. Ids first (client order), then tokens (text order).
/// The count check runs before any copy, so a `TooMany` leaves the store
/// untouched.
pub async fn collect(
    store: &UploadStore,
    convo: Uuid,
    cwd: &Path,
    tokens: Vec<String>,
    ids: Vec<Uuid>,
) -> Result<Vec<Attachment>, AttachError> {
    let limits = store.limits();
    let count = tokens.len() + ids.len();
    if count > limits.max_files as usize {
        return Err(AttachError::TooMany {
            count,
            max: limits.max_files,
        });
    }

    let mut out = Vec::with_capacity(count);
    for id in &ids {
        // `find` re-derives everything from disk — a persisted id with a
        // missing file is an unknown upload, full stop.
        out.push(
            store
                .find(convo, *id)
                .ok_or(AttachError::UnknownUpload(*id))?,
        );
    }
    for token in &tokens {
        out.push(token_to_attachment(store, convo, cwd, token).await?);
    }
    Ok(out)
}

async fn token_to_attachment(
    store: &UploadStore,
    convo: Uuid,
    cwd: &Path,
    token: &str,
) -> Result<Attachment, AttachError> {
    if token.starts_with("http://") || token.starts_with("https://") {
        return download_url(store, convo, token).await;
    }

    let path = resolve_path(cwd, token);
    let metadata = std::fs::metadata(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => AttachError::NotFound(path.clone()),
        _ => AttachError::Io(e),
    })?;
    if !metadata.is_file() {
        return Err(AttachError::NotAFile(path));
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let file = tokio::fs::File::open(&path).await?;
    store.store(convo, name, file).await
}

/// `./` resolves against the session cwd (not the process cwd — that was a
/// bug), `~/` against the home dir, absolute paths as-is.
fn resolve_path(cwd: &Path, token: &str) -> PathBuf {
    if token == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(token));
    }
    if let Some(rest) = token.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(token));
    }
    let p = Path::new(token);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Downloads a URL token into the store (amendment A1). The name is the
/// sanitized basename of the URL path ("file" fallback); `store`'s
/// `take(max+1)` is the size bound.
async fn download_url(
    store: &UploadStore,
    convo: Uuid,
    url: &str,
) -> Result<Attachment, AttachError> {
    let name = url_base_name(url);
    let limits = store.limits();

    // Content-Length is a fast fail: skip the download when the header
    // already proves oversize (the only path where `size` is known).
    let client = crate::http::client_builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| AttachError::Io(std::io::Error::other(e)))?;
    let response = client
        .get(url)
        .header("User-Agent", "PeakBot/1.0")
        .send()
        .await
        .map_err(|e| AttachError::Io(std::io::Error::other(e)))?;
    if !response.status().is_success() {
        return Err(AttachError::NotFound(PathBuf::from(url)));
    }
    let max_bytes = limits.max_file_mb as u64 * 1024 * 1024;
    if let Some(len) = response.content_length()
        && len > max_bytes
    {
        return Err(AttachError::TooLarge {
            name,
            size: Some(len),
            max_mb: limits.max_file_mb,
        });
    }

    // Stream straight into the store: `take(max+1)` bounds the copy, and a
    // mid-stream failure leaves no id dir behind. `Pin<Box<..>>` makes the
    // stream `Unpin` regardless of reqwest's internal pinning.
    let stream = response
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    let reader = StreamReader::new(Box::pin(stream));
    store.store(convo, &name, reader).await
}

/// Sanitized basename of the URL path — the query/fragment never leak into
/// the stored name; an empty basename falls back to "file" (via
/// `sanitize_name`).
fn url_base_name(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path = match after_scheme.find('/') {
        Some(i) => &after_scheme[i..],
        None => "",
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    sanitize_name(path.rsplit('/').next().unwrap_or(""))
}

/// Sanitize an untrusted file name for the store. Idempotent; never empty.
///
/// 1. Basename after the last `/` or `\` (kills `..` traversal).
/// 2. Control chars and `< > : " | ? *` → `_`.
/// 3. Trim leading/trailing whitespace and `.` (kills `.`/`..`, dotfiles,
///    Windows trailing dots).
/// 4. Windows reserved stems (`CON`, `COM1`…) get a `_` prefix.
/// 5. Truncate to 100 chars, keeping the extension — the root plus two UUIDs
///    is ~120 chars, so this keeps a typical full path under MAX_PATH.
/// 6. Empty → `file`.
pub fn sanitize_name(raw: &str) -> String {
    const MAX_NAME_CHARS: usize = 100;

    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut out: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    out = out
        .trim_matches(|c: char| c.is_whitespace() || c == '.')
        .to_string();
    if is_windows_reserved(&out) {
        out.insert(0, '_');
    }
    if out.chars().count() > MAX_NAME_CHARS {
        out = truncate_name(out, MAX_NAME_CHARS);
    }
    if out.is_empty() {
        "file".to_string()
    } else {
        out
    }
}

/// Windows treats everything before the FIRST dot as the name, so
/// `con.txt.bak` is reserved too.
fn is_windows_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    stem.strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|num| num.parse::<u8>().is_ok_and(|n| (1..=9).contains(&n)))
}

/// `<stem…>.<ext>` — the ellipsis marks the cut, and a re-sanitize is a
/// fixed point (no separator, no reserved stem, within the cap).
fn truncate_name(name: String, max_chars: usize) -> String {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) => (stem.to_string(), format!(".{ext}")),
        None => (name.clone(), String::new()),
    };
    let ext_len = ext.chars().count();
    if ext_len + 1 < max_chars {
        let stem_budget = max_chars - ext_len - 1;
        let keep: String = stem.chars().take(stem_budget).collect();
        format!("{keep}…{ext}")
    } else {
        // The extension alone cannot fit — just cap the whole name.
        name.chars().take(max_chars).collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    #[error("file not found: {}", .0.display())]
    NotFound(PathBuf),
    #[error("not a regular file: {}", .0.display())]
    NotAFile(PathBuf),
    /// `size` is `Some` only when a Content-Length header proved it before
    /// any bytes moved; the `take(max+1)` path knows only "over the limit".
    #[error("{name} is larger than {max_mb} MB")]
    TooLarge {
        name: String,
        size: Option<u64>,
        max_mb: u32,
    },
    #[error("too many files: {count} (max {max})")]
    TooMany { count: usize, max: u32 },
    #[error("attachment not found in this conversation — attach the file again")]
    UnknownUpload(Uuid),
    #[error(
        "inline data: images are no longer supported — reload the page and attach the file again"
    )]
    DataUriToken,
    #[error("invalid [img:] token: {0} (expected a path or http(s) URL)")]
    InvalidToken(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
