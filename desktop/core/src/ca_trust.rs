//! Trust of PeakBot's self-signed CA on macOS and Windows (CA-trust
//! design §4). Pure: parse the CA PEM, pick the user trust store for the
//! platform, and build/interpret the `security` / `certutil.exe`
//! commands. The controller runs them through `Sys::run`.
//!
//! No `cfg!` here: the platform arrives as a `Platform` value, so every
//! branch is unit-testable on any host.

use std::fmt;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sha1_smol::Sha1;

use crate::phase::Platform;
use crate::sys::RunOutput;

const BEGIN_CERT: &str = "-----BEGIN CERTIFICATE-----";
const END_CERT: &str = "-----END CERTIFICATE-----";

/// PeakBot's CA: where its PEM lives + its identity thumbprint.
pub struct PeakbotCa {
    path: String,
    thumbprint: String,
}

impl PeakbotCa {
    /// Parse the CA PEM at `path`. The thumbprint is the SHA-1 of the
    /// FIRST `CERTIFICATE` block's DER, 40 UPPERCASE hex. Err (a String)
    /// when the path is non-UTF-8, the PEM has no complete
    /// `BEGIN/END CERTIFICATE` block, or the body is not valid base64/DER.
    pub fn parse(path: &Path, pem: &str) -> Result<PeakbotCa, String> {
        // The path travels verbatim into the OS commands below.
        let path_str = path
            .to_str()
            .ok_or_else(|| format!("non-UTF-8 CA path: {}", path.display()))?;
        let Some(begin) = pem.find(BEGIN_CERT) else {
            return Err("no CERTIFICATE block in the CA PEM".into());
        };
        let body = &pem[begin + BEGIN_CERT.len()..];
        let Some(end) = body.find(END_CERT) else {
            return Err("CA PEM has no END CERTIFICATE line".into());
        };
        // Strip ALL whitespace (incl. \r) — PEM line wrapping is
        // irrelevant to the DER.
        let der_b64: String = body[..end].chars().filter(|c| !c.is_whitespace()).collect();
        let der = STANDARD
            .decode(der_b64.as_bytes())
            .map_err(|e| format!("CA PEM body is not valid base64: {e}"))?;
        // sha1_smol digests to lowercase hex; the stores want UPPER.
        let thumbprint = Sha1::from(der.as_slice())
            .digest()
            .to_string()
            .to_uppercase();
        Ok(PeakbotCa {
            path: path_str.to_string(),
            thumbprint,
        })
    }

    /// The path the CA PEM was parsed from.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// SHA-1 of the first CERTIFICATE block's DER; 40 UPPERCASE hex.
    pub fn thumbprint(&self) -> &str {
        &self.thumbprint
    }
}

/// The per-user trust store the CA is checked against / added to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustStore {
    /// The user's login keychain (`/usr/bin/security`).
    MacOs { login_keychain: PathBuf },
    /// The user's `Root` certificate store (`certutil.exe -user`).
    Windows,
}

/// One ready-to-run trust-store command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustCmd {
    pub program: &'static str,
    pub args: Vec<String>,
}

impl fmt::Display for TrustCmd {
    /// `"{program} {args joined by ' '}"` — the form the failure detail
    /// shows the user.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.program, self.args.join(" "))
    }
}

impl TrustStore {
    /// The trust store for this platform + home dir: Linux ⇒ `None`
    /// (the webview trusts the CA itself), Windows ⇒ `Windows`,
    /// MacOs ⇒ `MacOs { login_keychain: home/Library/Keychains/login.keychain-db }`.
    pub fn for_platform(p: Platform, home: &Path) -> Option<TrustStore> {
        match p {
            Platform::Linux => None,
            Platform::Windows => Some(TrustStore::Windows),
            Platform::MacOs => Some(TrustStore::MacOs {
                login_keychain: home.join("Library/Keychains/login.keychain-db"),
            }),
        }
    }

    /// The command that checks whether the CA is trusted.
    pub fn check_cmd(&self, ca: &PeakbotCa) -> TrustCmd {
        match self {
            TrustStore::MacOs { .. } => TrustCmd {
                program: "/usr/bin/security",
                args: vec![
                    "verify-cert".into(),
                    "-c".into(),
                    ca.path().into(),
                    "-p".into(),
                    "ssl".into(),
                    "-l".into(),
                    "-L".into(),
                    "-R".into(),
                    "offline".into(),
                ],
            },
            // certutil can only look a cert up by its hash.
            TrustStore::Windows => TrustCmd {
                program: "certutil.exe",
                args: vec![
                    "-user".into(),
                    "-store".into(),
                    "Root".into(),
                    ca.thumbprint().into(),
                ],
            },
        }
    }

    /// Interpret the check command's output: trusted or not.
    pub fn is_trusted(&self, ca: &PeakbotCa, check: &RunOutput) -> bool {
        match self {
            // verify-cert is silent and exact: the exit code is the answer.
            TrustStore::MacOs { .. } => check.code == Some(0),
            // certutil exits 0 for a store listing even when the cert is
            // absent, so the thumbprint must appear in the output too.
            TrustStore::Windows => {
                let stdout: String = check
                    .stdout
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                check.code == Some(0) && stdout.to_uppercase().contains(ca.thumbprint())
            }
        }
    }

    /// The command that adds the CA to the trust store.
    pub fn add_cmd(&self, ca: &PeakbotCa) -> TrustCmd {
        match self {
            TrustStore::MacOs { login_keychain } => TrustCmd {
                program: "/usr/bin/security",
                args: vec![
                    "add-trusted-cert".into(),
                    "-r".into(),
                    "trustRoot".into(),
                    "-p".into(),
                    "ssl".into(),
                    "-k".into(),
                    login_keychain.to_string_lossy().into_owned(),
                    ca.path().into(),
                ],
            },
            TrustStore::Windows => TrustCmd {
                program: "certutil.exe",
                args: vec![
                    "-user".into(),
                    "-addstore".into(),
                    "Root".into(),
                    ca.path().into(),
                ],
            },
        }
    }
}
