//! Environment (DESIGN §4.9/§4.10).

use std::ffi::{OsStr, OsString};
use std::path::Path;

/// AppImage plumbing removal for the sidecar env (DESIGN §4.9): if
/// `APPDIR` is set, drop `APPDIR, APPIMAGE, ARGV0, OWD`; strip `:`-entries
/// under `$APPDIR` from the list-path vars (drop the var if empty); drop a
/// `FONTCONFIG_FILE` naming the app's own generated config (DESIGN §4.12);
/// drop any other var whose value contains `$APPDIR`. No `APPDIR` → unchanged
/// (order preserved).
pub fn sanitize_env(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let vars: Vec<(OsString, OsString)> = parent.into_iter().collect();
    let appdir: Option<OsString> = vars
        .iter()
        .find(|(k, _)| k == "APPDIR")
        .map(|(_, v)| v.clone());
    let Some(appdir) = appdir else {
        return vars;
    };
    // `$APPDIR/` — the prefix for "under APPDIR" (the exact dir counts too).
    let appdir_slash = {
        let mut s = appdir.clone();
        s.push(OsStr::new("/"));
        s
    };
    const DROPPED: [&str; 4] = ["APPDIR", "APPIMAGE", "ARGV0", "OWD"];
    const LIST_PATHS: [&str; 6] = [
        "PATH",
        "LD_LIBRARY_PATH",
        "XDG_DATA_DIRS",
        "GST_PLUGIN_SYSTEM_PATH",
        "GST_PLUGIN_PATH",
        "GIO_EXTRA_MODULES",
    ];
    vars.into_iter()
        .filter_map(|(k, v)| {
            let key = k.to_str().unwrap_or("");
            if DROPPED.contains(&key) {
                return None;
            }
            // DESIGN §4.12: the generated config names a pid-scoped path
            // that dies with the app — the sidecar uses the host fontconfig.
            if key == "FONTCONFIG_FILE"
                && v.to_str().is_some_and(|s| {
                    Path::new(s)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(crate::fontconfig::GENERATED_CONF_PREFIX))
                })
            {
                return None;
            }
            if LIST_PATHS.contains(&key) {
                match v.to_str() {
                    // Keep only the entries that are not under $APPDIR.
                    Some(s) => {
                        let kept: Vec<&str> = s
                            .split(':')
                            .filter(|e| {
                                let os = OsStr::new(e);
                                *os != *appdir.as_os_str()
                                    && !os
                                        .as_encoded_bytes()
                                        .starts_with(appdir_slash.as_encoded_bytes())
                            })
                            .collect();
                        if kept.is_empty() {
                            return None;
                        }
                        return Some((k, OsString::from(kept.join(":"))));
                    }
                    // Non-UTF-8 list value: nothing to strip; keep as-is.
                    None => return Some((k, v)),
                }
            }
            // Any other var whose value contains $APPDIR is dropped.
            if v.as_os_str()
                .to_str()
                .is_some_and(|s| appdir.to_str().is_some_and(|a| s.contains(a)))
            {
                return None;
            }
            Some((k, v))
        })
        .collect()
}

/// `WEBKIT_DISABLE_DMABUF_RENDERER=1` when the var is unset (Linux
/// webkit fix, DESIGN §4.10); `None` when already set to anything.
pub fn webkit_dmabuf_default(existing: Option<&OsStr>) -> Option<(&'static str, &'static str)> {
    match existing {
        None => Some(("WEBKIT_DISABLE_DMABUF_RENDERER", "1")),
        Some(_) => None,
    }
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

    // DESIGN §4.12 t133: the generated config must not leak into the
    // sidecar env (the sidecar uses the host's fontconfig, and the
    // generated file names a pid-scoped path that dies with the shell).
    #[test]
    fn t133_sanitize_env_drops_generated_fontconfig_file_under_appdir() {
        use crate::fontconfig::GENERATED_CONF_PREFIX;
        let generated = format!("/run/user/1000/{GENERATED_CONF_PREFIX}7.conf");
        let input = kv(&[
            ("APPDIR", "/tmp/.mount_ab"),
            ("FONTCONFIG_FILE", generated.as_str()),
            ("HOME", "/home/u"),
        ]);
        let out = sanitize_env(input);
        let got: Vec<(&str, &str)> = out
            .iter()
            .map(|(k, v)| (k.to_str().unwrap(), v.to_str().unwrap()))
            .collect();
        assert!(
            !got.iter().any(|(k, _)| *k == "FONTCONFIG_FILE"),
            "the generated FONTCONFIG_FILE must be dropped under APPDIR; got {got:?}"
        );
        assert!(
            got.iter().any(|(k, v)| *k == "HOME" && *v == "/home/u"),
            "unrelated vars must be untouched; got {got:?}"
        );
    }

    // DESIGN §4.12 t134: a user's OWN fontconfig file is never touched,
    // and without APPDIR the generated name is kept (identity).
    #[test]
    fn t134_sanitize_env_keeps_foreign_or_user_fontconfig_file() {
        use crate::fontconfig::GENERATED_CONF_PREFIX;
        let generated = format!("/run/user/1000/{GENERATED_CONF_PREFIX}9.conf");

        // APPDIR set + a user's own config ⇒ kept.
        let input = kv(&[
            ("APPDIR", "/tmp/.mount_ab"),
            ("FONTCONFIG_FILE", "/home/u/my.conf"),
            ("HOME", "/home/u"),
        ]);
        let out = sanitize_env(input);
        assert!(
            out.iter()
                .any(|(k, v)| k == "FONTCONFIG_FILE" && v == "/home/u/my.conf"),
            "a non-generated FONTCONFIG_FILE must be kept under APPDIR; got {out:?}"
        );

        // No APPDIR + the generated name ⇒ identity (kept, unchanged).
        let input = kv(&[("FONTCONFIG_FILE", generated.as_str()), ("HOME", "/home/u")]);
        let out = sanitize_env(input.clone());
        assert_eq!(out, input, "no APPDIR ⇒ unchanged; got {out:?}");
    }
}
