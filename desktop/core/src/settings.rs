//! The desktop's settings model (design #377 §3.1): the `Settings` sum
//! type (illegal combos unrepresentable), boundary validation
//! (`TryFrom<SettingsWire>`), file parse/render/load, and the
//! `settings_view` for the settings window.
//!
//! Load policy (criterion 9): missing/corrupt/unknown-version files load
//! as the defaults with one warning, and load never writes (T11).

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::phase::{StatusView, TrayModel};
use crate::token::{self, Token};

/// The settings file version; a mismatch loads as the defaults.
pub const SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http,
    Https,
}

impl Protocol {
    pub fn scheme(self) -> &'static str {
        match self {
            Protocol::Http => "http",
            Protocol::Https => "https",
        }
    }
}

/// The validated model. Illegal combos are unrepresentable:
/// `Remote` ⇒ HTTPS ∧ token by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settings {
    Loopback { protocol: Protocol, token: bool },
    Remote,
}

impl Default for Settings {
    /// Today's behaviour: http loopback, token on.
    fn default() -> Self {
        Settings::Loopback {
            protocol: Protocol::Http,
            token: true,
        }
    }
}

impl Settings {
    /// `Remote` ⇒ `Https` by construction.
    pub fn protocol(self) -> Protocol {
        match self {
            Settings::Loopback { protocol, .. } => protocol,
            Settings::Remote => Protocol::Https,
        }
    }

    /// `Remote` ⇒ true by construction.
    pub fn token(self) -> bool {
        match self {
            Settings::Loopback { token, .. } => token,
            Settings::Remote => true,
        }
    }

    pub fn remote(self) -> bool {
        matches!(self, Settings::Remote)
    }

    pub fn bind_host(self) -> &'static str {
        match self {
            Settings::Loopback { .. } => "127.0.0.1",
            Settings::Remote => "0.0.0.0",
        }
    }
}

/// Untrusted flat shape: the IPC payload (both directions) and the file
/// body. Unknown fields are IGNORED (serde default), so newer files with
/// extra keys still load. Missing fields are a parse error, so the file
/// loads as defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsWire {
    pub protocol: Protocol,
    pub token: bool,
    pub remote: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsError {
    RemoteNeedsHttps,
    RemoteNeedsToken,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SettingsError::RemoteNeedsHttps => "Remote access requires HTTPS.",
            SettingsError::RemoteNeedsToken => "Remote access requires the access token.",
        })
    }
}

impl std::error::Error for SettingsError {}

/// THE boundary check: every legal wire value maps to a `Settings`,
/// every illegal one is rejected (a missing HTTPS wins over a missing
/// token).
impl TryFrom<SettingsWire> for Settings {
    type Error = SettingsError;

    fn try_from(w: SettingsWire) -> Result<Self, Self::Error> {
        if !w.remote {
            return Ok(Settings::Loopback {
                protocol: w.protocol,
                token: w.token,
            });
        }
        if w.protocol != Protocol::Https {
            return Err(SettingsError::RemoteNeedsHttps);
        }
        if !w.token {
            return Err(SettingsError::RemoteNeedsToken);
        }
        Ok(Settings::Remote)
    }
}

impl From<Settings> for SettingsWire {
    fn from(s: Settings) -> Self {
        SettingsWire {
            protocol: s.protocol(),
            token: s.token(),
            remote: s.remote(),
        }
    }
}

/// `{"v":1, ...SettingsWire}`.
#[derive(Debug, Serialize, Deserialize)]
struct SettingsFile {
    v: u32,
    #[serde(flatten)]
    s: SettingsWire,
}

/// Compact JSON + a trailing newline. The file is the single source of
/// desired state.
pub fn render_settings_file(s: Settings) -> String {
    let file = SettingsFile {
        v: SETTINGS_VERSION,
        s: SettingsWire::from(s),
    };
    format!(
        "{}\n",
        serde_json::to_string(&file).expect("a SettingsWire is always serialisable")
    )
}

/// JSON error | `"unsupported settings version {v}"` | the
/// `SettingsError` text.
pub fn parse_settings_file(text: &str) -> Result<Settings, String> {
    let file: SettingsFile =
        serde_json::from_str(text).map_err(|e| format!("invalid settings file: {e}"))?;
    if file.v != SETTINGS_VERSION {
        return Err(format!("unsupported settings version {}", file.v));
    }
    Settings::try_from(file.s).map_err(|e| e.to_string())
}

/// `None` ⇒ the default; any parse/validation error ⇒ one warning + the
/// default. Load never writes.
pub fn load_settings(text: Option<&str>) -> Settings {
    match text {
        None => Settings::default(),
        Some(t) => match parse_settings_file(t) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("loading settings: {e} — using defaults");
                Settings::default()
            }
        },
    }
}

/// THE save payload (design #377b §3.2): the validated settings plus an
/// optional new token value. Illegal state unrepresentable — `new_token`
/// is `Some` ⇒ `settings.token()`. Fields are private; only `parse` and
/// `From<Settings>` build it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveRequest {
    settings: Settings,
    /// `None` = keep the current token, generate one if absent.
    new_token: Option<Token>,
}

impl SaveRequest {
    /// 1. `Settings::try_from(wire)` — a settings error wins (step 1);
    /// 2. token off ⇒ the token field is ignored, never an error;
    /// 3. token on ⇒ `parse_new_token` (empty ⇒ keep/generate).
    pub fn parse(wire: SettingsWire, token: &str) -> Result<SaveRequest, String> {
        let settings = Settings::try_from(wire).map_err(|e| e.to_string())?;
        let new_token = if settings.token() {
            token::parse_new_token(token).map_err(|e| e.to_string())?
        } else {
            None
        };
        Ok(SaveRequest {
            settings,
            new_token,
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// `None` = keep the current token, generate one if absent.
    pub fn new_token(&self) -> Option<&Token> {
        self.new_token.as_ref()
    }
}

impl From<Settings> for SaveRequest {
    /// "Save these, keep the token."
    fn from(settings: Settings) -> Self {
        SaveRequest {
            settings,
            new_token: None,
        }
    }
}

/// What the settings window renders: the saved wire value, the current
/// token, and the live tray/status state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingsView {
    pub settings: SettingsWire,
    /// Some iff the token is on AND the web-token file parses; JSON
    /// `string`/`null` for the page.
    pub token: Option<Token>,
    /// The app window can trust PeakBot's CA on Linux only (design
    /// #377b §3.2): the page shows the macOS certificate note when true.
    pub cert_warning: bool,
    /// The background row is shown only when true (Q1: hidden on
    /// Windows in v1); = `TrayModel.background_visible`.
    pub background_visible: bool,
    pub background: bool,
    pub background_enabled: bool,
    /// `status.kind == "starting"`.
    pub busy: bool,
    pub web_token_path: String,
}

pub fn settings_view(
    s: Settings,
    token: Option<Token>,
    tray: &TrayModel,
    status: &StatusView,
    web_token: &Path,
) -> SettingsView {
    SettingsView {
        settings: s.into(),
        // The view drops the token when the token is off.
        token: if s.token() { token } else { None },
        cert_warning: !cfg!(target_os = "linux"),
        background_visible: tray.background_visible,
        background: tray.background_checked,
        background_enabled: tray.background_enabled,
        busy: status.kind == "starting",
        web_token_path: web_token.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::phase::{StatusView, TrayModel};
    use crate::settings::{
        Protocol, SETTINGS_VERSION, SaveRequest, Settings, SettingsError, SettingsWire,
        load_settings, parse_settings_file, render_settings_file, settings_view,
    };
    use crate::token::Token;

    fn wire(protocol: Protocol, token: bool, remote: bool) -> SettingsWire {
        SettingsWire {
            protocol,
            token,
            remote,
        }
    }

    // Design #377 t66: the default is today's behaviour and the render is
    // byte-exact (the file is the single source of desired state).
    #[test]
    fn t66_default_and_exact_render() {
        assert_eq!(SETTINGS_VERSION, 1);
        let d = Settings::default();
        assert_eq!(
            d,
            Settings::Loopback {
                protocol: Protocol::Http,
                token: true
            },
            "default = today's behaviour: http loopback, token on"
        );
        assert_eq!(
            render_settings_file(d),
            "{\"v\":1,\"protocol\":\"http\",\"token\":true,\"remote\":false}\n"
        );
        assert_eq!(
            render_settings_file(Settings::Remote),
            "{\"v\":1,\"protocol\":\"https\",\"token\":true,\"remote\":true}\n"
        );
    }

    // Design #377 t67: THE boundary check — every legal wire value maps
    // to a `Settings`, every illegal one is rejected with the locked
    // message.
    #[test]
    fn t67_try_from_wire_validates_the_security_invariant() {
        // The four loopback combos are all legal (token optional).
        assert_eq!(
            Settings::try_from(wire(Protocol::Http, true, false)),
            Ok(Settings::Loopback {
                protocol: Protocol::Http,
                token: true
            })
        );
        assert_eq!(
            Settings::try_from(wire(Protocol::Http, false, false)),
            Ok(Settings::Loopback {
                protocol: Protocol::Http,
                token: false
            })
        );
        assert_eq!(
            Settings::try_from(wire(Protocol::Https, true, false)),
            Ok(Settings::Loopback {
                protocol: Protocol::Https,
                token: true
            })
        );
        assert_eq!(
            Settings::try_from(wire(Protocol::Https, false, false)),
            Ok(Settings::Loopback {
                protocol: Protocol::Https,
                token: false
            })
        );
        // Remote ⇒ HTTPS ∧ token, by construction.
        assert_eq!(
            Settings::try_from(wire(Protocol::Https, true, true)),
            Ok(Settings::Remote)
        );

        // Locked error messages (asserted verbatim).
        let err = Settings::try_from(wire(Protocol::Http, true, true)).unwrap_err();
        assert_eq!(err, SettingsError::RemoteNeedsHttps);
        assert_eq!(err.to_string(), "Remote access requires HTTPS.");

        let err = Settings::try_from(wire(Protocol::Https, false, true)).unwrap_err();
        assert_eq!(err, SettingsError::RemoteNeedsToken);
        assert_eq!(err.to_string(), "Remote access requires the access token.");

        // Check order: a missing HTTPS wins over a missing token.
        let err = Settings::try_from(wire(Protocol::Http, false, true)).unwrap_err();
        assert_eq!(err, SettingsError::RemoteNeedsHttps);
        assert_eq!(err.to_string(), "Remote access requires HTTPS.");
    }

    // Design #377 t68: render→parse roundtrip for all 5 valid values.
    #[test]
    fn t68_render_parse_roundtrip_all_five_valid_values() {
        let all = [
            Settings::Loopback {
                protocol: Protocol::Http,
                token: true,
            },
            Settings::Loopback {
                protocol: Protocol::Http,
                token: false,
            },
            Settings::Loopback {
                protocol: Protocol::Https,
                token: true,
            },
            Settings::Loopback {
                protocol: Protocol::Https,
                token: false,
            },
            Settings::Remote,
        ];
        for s in all {
            assert_eq!(
                parse_settings_file(&render_settings_file(s)),
                Ok(s),
                "roundtrip for {s:?}"
            );
        }
    }

    // Design #377 t69 (criterion 9): every bad file shape loads as the
    // default; unknown extra keys are ignored; load never writes.
    #[test]
    fn t69_load_falls_back_to_defaults_on_bad_files() {
        let d = Settings::default();
        assert_eq!(load_settings(None), d, "missing file ⇒ default");
        assert_eq!(load_settings(Some("")), d, "empty file ⇒ default");
        assert_eq!(load_settings(Some("{")), d, "non-JSON ⇒ default");
        assert_eq!(load_settings(Some("null")), d, "JSON null ⇒ default");
        assert_eq!(
            load_settings(Some("{\"v\":1,\"protocol\":\"http\"}")),
            d,
            "a missing field ⇒ default"
        );
        assert_eq!(
            load_settings(Some(
                "{\"v\":0,\"protocol\":\"http\",\"token\":true,\"remote\":false}"
            )),
            d,
            "v:0 ⇒ default"
        );
        assert_eq!(
            load_settings(Some(
                "{\"v\":2,\"protocol\":\"http\",\"token\":true,\"remote\":false}"
            )),
            d,
            "v:2 ⇒ default"
        );
        assert_eq!(
            load_settings(Some(
                "{\"v\":1,\"protocol\":\"http\",\"token\":false,\"remote\":true}"
            )),
            d,
            "an illegal combo ⇒ default"
        );

        // The version error is specific, not a generic parse failure.
        assert_eq!(
            parse_settings_file("{\"v\":2,\"protocol\":\"http\",\"token\":true,\"remote\":false}"),
            Err("unsupported settings version 2".to_string())
        );

        // Unknown extra keys are ignored (a newer app's file still loads).
        assert_eq!(
            load_settings(Some(
                "{\"v\":1,\"protocol\":\"https\",\"token\":true,\"remote\":false,\"future_key\":42}"
            )),
            Settings::Loopback {
                protocol: Protocol::Https,
                token: true
            }
        );
        // A valid file loads its values.
        assert_eq!(
            load_settings(Some(
                "{\"v\":1,\"protocol\":\"https\",\"token\":false,\"remote\":false}"
            )),
            Settings::Loopback {
                protocol: Protocol::Https,
                token: false
            }
        );
    }

    // Design #377 t70: the accessors derive every launch parameter from
    // the one value (Remote ⇒ https + token + 0.0.0.0 by construction).
    #[test]
    fn t70_accessors_and_wire_mapping() {
        let s = Settings::Loopback {
            protocol: Protocol::Http,
            token: true,
        };
        assert_eq!(s.protocol(), Protocol::Http);
        assert!(s.token());
        assert!(!s.remote());
        assert_eq!(s.bind_host(), "127.0.0.1");

        let s = Settings::Loopback {
            protocol: Protocol::Https,
            token: false,
        };
        assert_eq!(s.protocol(), Protocol::Https);
        assert!(!s.token());
        assert!(!s.remote());
        assert_eq!(s.bind_host(), "127.0.0.1");

        let s = Settings::Remote;
        assert_eq!(
            s.protocol(),
            Protocol::Https,
            "Remote ⇒ HTTPS by construction"
        );
        assert!(s.token(), "Remote ⇒ token by construction");
        assert!(s.remote());
        assert_eq!(s.bind_host(), "0.0.0.0");

        assert_eq!(Protocol::Http.scheme(), "http");
        assert_eq!(Protocol::Https.scheme(), "https");

        assert_eq!(
            SettingsWire::from(Settings::Remote),
            wire(Protocol::Https, true, true)
        );
        assert_eq!(
            SettingsWire::from(Settings::Loopback {
                protocol: Protocol::Http,
                token: false
            }),
            wire(Protocol::Http, false, false)
        );
    }

    fn tray() -> TrayModel {
        TrayModel {
            status: "PeakBot 0.23.0 · running (port 7824)".into(),
            background_visible: true,
            background_checked: true,
            background_enabled: false,
            background_label: "Keep PeakBot running in background".into(),
        }
    }

    fn status(kind: &'static str) -> StatusView {
        StatusView {
            kind,
            title: String::new(),
            detail: None,
            actions: Vec::new(),
        }
    }

    // Design #377 t74 (updated for #377b: the view takes the file
    // token as a new argument): the view is a pure mapping of saved
    // settings + tray + status + token path; busy ⇔ kind == "starting".
    #[test]
    fn t74_settings_view_maps_tray_status_and_path() {
        let s = Settings::Loopback {
            protocol: Protocol::Http,
            token: false,
        };
        let tray = tray();
        let web_token = Path::new("/cfg/web-token");

        let v = settings_view(s, None, &tray, &status("ready"), web_token);
        assert_eq!(
            v.settings,
            wire(Protocol::Http, false, false),
            "the saved wire value"
        );
        assert!(v.background, "= tray.background_checked");
        assert!(!v.background_enabled, "= tray.background_enabled");
        assert!(!v.busy, "kind \"ready\" ⇒ not busy");
        assert_eq!(v.web_token_path, "/cfg/web-token");

        let v = settings_view(s, None, &tray, &status("starting"), web_token);
        assert!(v.busy, "busy ⇔ status.kind == \"starting\"");
    }

    // Design #377b t92: SaveRequest::parse — ONE validated save
    // payload. The settings error wins over the token error; a token
    // value can only exist in a request whose settings say "token on";
    // the token never reaches the file body.
    #[test]
    fn t92_save_request_parse_validates_settings_then_token() {
        // Step 1 wins: an illegal wire is a settings error even when
        // the token is bad too.
        assert_eq!(
            SaveRequest::parse(wire(Protocol::Http, true, true), "not a valid token!"),
            Err("Remote access requires HTTPS.".to_string())
        );

        // Token off: the field is ignored, never an error (even a
        // 1-char "token" is fine — it is never looked at).
        let req =
            SaveRequest::parse(wire(Protocol::Http, false, false), "x").expect("token off is ok");
        assert_eq!(
            req.settings(),
            Settings::Loopback {
                protocol: Protocol::Http,
                token: false
            }
        );
        assert_eq!(req.new_token(), None, "token off ⇒ no token value");

        // Token on + empty ⇒ "keep the current token, or generate one".
        let req = SaveRequest::parse(wire(Protocol::Http, true, false), "").expect("empty ⇒ keep");
        assert_eq!(req.new_token(), None);

        // Token on + valid ⇒ Some; surrounding whitespace is trimmed.
        let req = SaveRequest::parse(wire(Protocol::Http, true, false), "  abc1234567890123  ")
            .expect("a valid token parses");
        assert_eq!(
            req.new_token(),
            Some(&Token::parse("abc1234567890123").expect("parses"))
        );

        // Token on + 15 chars ⇒ the locked TooShort text.
        assert_eq!(
            SaveRequest::parse(wire(Protocol::Https, true, true), &"a".repeat(15)),
            Err("The access token must be at least 16 characters.".to_string())
        );

        // The token never reaches the file body (invariant I8).
        let req = SaveRequest::parse(wire(Protocol::Http, true, false), "abc1234567890123")
            .expect("valid");
        let file = render_settings_file(req.settings());
        assert!(
            !file.contains("abc1234567890123"),
            "the rendered settings file must not carry the token; got {file:?}"
        );

        // From<Settings>: "save these, keep the token".
        let req = SaveRequest::from(Settings::Remote);
        assert_eq!(req.settings(), Settings::Remote);
        assert_eq!(req.new_token(), None);
    }

    // Design #377b t93: the view carries the token to the settings
    // window — Some iff the token is on AND the file parses — plus the
    // platform cert warning (the app window can trust the CA on Linux
    // only).
    #[test]
    fn t93_settings_view_carries_token_and_cert_warning() {
        let on = Settings::Loopback {
            protocol: Protocol::Http,
            token: true,
        };
        let off = Settings::Loopback {
            protocol: Protocol::Http,
            token: false,
        };
        let tok = Token::parse("abc1234567890123").expect("parses");
        let tray = tray();
        let p = Path::new("/cfg/web-token");

        // on + a parseable file token ⇒ Some.
        let v = settings_view(on, Some(tok.clone()), &tray, &status("ready"), p);
        assert_eq!(
            v.token.as_ref().map(|t| t.as_str()),
            Some("abc1234567890123"),
            "token on + file token ⇒ the view carries it"
        );
        // off + a file token ⇒ dropped (the checkbox is off).
        let v = settings_view(off, Some(tok.clone()), &tray, &status("ready"), p);
        assert!(
            v.token.is_none(),
            "the view drops the token when the token is off"
        );
        // on + no file token ⇒ None (the field starts empty ⇒ generate).
        let v = settings_view(on, None, &tray, &status("ready"), p);
        assert!(v.token.is_none(), "no file token ⇒ None");

        // JSON: "token":null / "token":"…" (the page renders it as-is).
        let json = serde_json::to_value(settings_view(on, None, &tray, &status("ready"), p))
            .expect("the view serialises");
        assert_eq!(json["token"], serde_json::Value::Null);
        let json = serde_json::to_value(settings_view(
            on,
            Some(tok.clone()),
            &tray,
            &status("ready"),
            p,
        ))
        .expect("the view serialises");
        assert_eq!(json["token"], serde_json::json!("abc1234567890123"));

        // The app window can trust the CA on Linux only.
        assert_eq!(
            settings_view(on, None, &tray, &status("ready"), p).cert_warning,
            !cfg!(target_os = "linux")
        );
    }
}
