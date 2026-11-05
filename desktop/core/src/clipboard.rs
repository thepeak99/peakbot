//! The `clipboard_image_png` size bound (desktop/DESIGN.md §15): pure, so it
//! is tested without GTK.

/// Hard bound on the clipboard PNG crossing IPC — the default
/// `uploads.max_file_mb`. Larger images are refused, never truncated.
pub const MAX_PNG_MB: usize = 50;

/// `png` unchanged when it is at most `MAX_PNG_MB` MiB, else the reason.
/// Empty input passes through (it is the "no image" wire value).
pub fn checked_png(png: Vec<u8>) -> Result<Vec<u8>, String> {
    if png.len() > MAX_PNG_MB * 1024 * 1024 {
        return Err(format!("clipboard image is larger than {MAX_PNG_MB} MB"));
    }
    Ok(png)
}
