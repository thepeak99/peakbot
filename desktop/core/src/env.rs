//! Environment (DESIGN §4.9/§4.10).

use std::ffi::{OsStr, OsString};

/// AppImage plumbing removal for the sidecar env (DESIGN §4.9): if
/// `APPDIR` is set, drop `APPDIR, APPIMAGE, ARGV0, OWD`; strip `:`-entries
/// under `$APPDIR` from the list-path vars (drop the var if empty); drop
/// any other var whose value contains `$APPDIR`. No `APPDIR` → unchanged
/// (order preserved).
pub fn sanitize_env(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    todo!()
}

/// `WEBKIT_DISABLE_DMABUF_RENDERER=1` when the var is unset (Linux
/// webkit fix, DESIGN §4.10); `None` when already set to anything.
pub fn webkit_dmabuf_default(existing: Option<&OsStr>) -> Option<(&'static str, &'static str)> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(*k), OsString::from(*v)))
            .collect()
    }

    // §9.1 case 22.
    #[test]
    fn t22_sanitize_env_without_appdir_is_identity() {
        let input = kv(&[("HOME", "/home/u"), ("PATH", "/usr/bin"), ("DISPLAY", ":0")]);
        let out = sanitize_env(input.clone());
        assert_eq!(out, input, "no APPDIR ⇒ unchanged, order preserved");
    }

    // §9.1 case 23.
    #[test]
    fn t23_sanitize_env_strips_appimage_plumbing() {
        let input = kv(&[
            ("APPDIR", "/tmp/.mount_ab"),
            ("APPIMAGE", "/tmp/x.AppImage"),
            ("ARGV0", "peakbot-desktop"),
            ("OWD", "/tmp"),
            ("PATH", "/tmp/.mount_ab/usr/bin:/usr/bin"),
            ("LD_LIBRARY_PATH", "/tmp/.mount_ab/usr/lib"),
            ("GTK_PATH", "/tmp/.mount_ab/usr/lib/gtk-3.0"),
            ("HOME", "/home/u"),
            ("DISPLAY", ":0"),
        ]);
        let out = sanitize_env(input);
        let got: Vec<(&str, &str)> = out
            .iter()
            .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
            .collect();

        for dropped in [
            "APPDIR",
            "APPIMAGE",
            "ARGV0",
            "OWD",
            "LD_LIBRARY_PATH",
            "GTK_PATH",
        ] {
            assert!(
                !got.iter().any(|(k, _)| *k == dropped),
                "{dropped} must be removed; got {got:?}"
            );
        }
        let path = got
            .iter()
            .find(|(k, _)| *k == "PATH")
            .map(|(_, v)| *v)
            .unwrap_or("");
        assert_eq!(
            path, "/usr/bin",
            "PATH must keep only the non-APPDIR entries; got {path:?}"
        );
        assert!(
            got.iter().any(|(k, v)| *k == "HOME" && *v == "/home/u"),
            "HOME must be untouched; got {got:?}"
        );
        assert!(
            got.iter().any(|(k, v)| *k == "DISPLAY" && *v == ":0"),
            "DISPLAY must be untouched; got {got:?}"
        );
    }

    // §9.1 case 24.
    #[test]
    fn t24_webkit_dmabuf_default() {
        assert_eq!(
            webkit_dmabuf_default(None),
            Some(("WEBKIT_DISABLE_DMABUF_RENDERER", "1"))
        );
        assert_eq!(webkit_dmabuf_default(Some(OsStr::new("0"))), None);
    }
}
