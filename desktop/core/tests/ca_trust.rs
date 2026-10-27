//! CA-trust design §4 — `core/src/ca_trust.rs` contract.
//!
//! The fixtures are REAL self-signed certs generated with openssl
//! (`req -x509`), cert PEM only — the private keys are NOT in the repo.
//! `FIXTURE_SHA1` / `SECOND_SHA1` come from
//! `openssl x509 -noout -fingerprint -sha1 | tr -d ':'` (uppercased).
//!
//! RED until the `ca_trust` module is implemented: every test calls a
//! `todo!()` stub and panics at runtime.

use std::path::{Path, PathBuf};

use peakbot_desktop_core::ca_trust::{PeakbotCa, TrustCmd, TrustStore};
use peakbot_desktop_core::phase::Platform;
use peakbot_desktop_core::sys::RunOutput;

/// The fixture CA cert (openssl, 10-year self-signed).
const FIXTURE_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIDJTCCAg2gAwIBAgIUWQWuHFoWUzh61plJAyaRzNKgpbUwDQYJKoZIhvcNAQEL
BQAwIjEgMB4GA1UEAwwXUGVha0JvdCBUZXN0IEZpeHR1cmUgQ0EwHhcNMjYxMDI3
MTQwNzA4WhcNMzYxMDI0MTQwNzA4WjAiMSAwHgYDVQQDDBdQZWFrQm90IFRlc3Qg
Rml4dHVyZSBDQTCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAIXf4iIa
Z9Z0QXgRZD9ccGVEilX09MUaJcKnNo9RR9h/5rgWEn0sPbVDz/0IySxICzjdL5ek
F30Xu/E13nXCcL5yFxBTA9VmkW2ZLj5Bn/pNZGfWeQeqZ7oFdOR8Iw75XeAjowjI
1g0ZB8iM1JyuuaGJjtHWlJ8vpOY0VZEB/jvlSkVZ4NqvjT6f3h7S6USUhaNW6EFI
LBKycWUZ8mWoL9Ly5jB+C6kR5geGHl8lK/1Z7RyqyEh2J6sp1iUvtqEOZR6NFfCy
5eBE+BdUgvO0Gz6vfC4tXvpxWYSc0/dtSkq1rsT3HBoXuIgEo7n/LZ6pQqqBip1b
tBZbjSzAE8t0AwECAwEAAaNTMFEwHQYDVR0OBBYEFBay38etvAkZau3PFkDys+zy
0y3pMB8GA1UdIwQYMBaAFBay38etvAkZau3PFkDys+zy0y3pMA8GA1UdEwEB/wQF
MAMBAf8wDQYJKoZIhvcNAQELBQADggEBAAw/cvPHEKiatiif+8XMoTyvTAG0nvma
hhpnmeaQz3It2hnnJrgiEVY4m16o4AMjU9CZmE/isCpJKMVbmK4aRW+G4FyaLtF/
rjD4gl0IRzgmzEDOOLhqpH34nliJNANHTWJxSE3qXTiACxxEAkrnijaihCCU/2Ta
M2J9RlmrmvFDXhAO4y6VGXRhAxPrBs5yEQ5kvlON17L61ChdmIuH9YdlopsQiJEX
zUm22DjFCRmsEF7T2nVXKFKuWmRc2DcTfK+jfPmJL27yTPso1ICK30wPJpDsGCkI
Mau1Qxq3S7SfaVNWM0OeBd9HV3682/Fu/7MHE+ATQV5Ved7dQCfVoVc=
-----END CERTIFICATE-----
"#;

/// A second, DIFFERENT self-signed cert — for the two-blocks test.
const SECOND_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIDLTCCAhWgAwIBAgIUfB3z8AF3KSKDqCFttTERrJtdsnswDQYJKoZIhvcNAQEL
BQAwJjEkMCIGA1UEAwwbUGVha0JvdCBUZXN0IEZpeHR1cmUgU2Vjb25kMB4XDTI2
MTAyNzE0MDcwOFoXDTM2MTAyNDE0MDcwOFowJjEkMCIGA1UEAwwbUGVha0JvdCBU
ZXN0IEZpeHR1cmUgU2Vjb25kMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKC
AQEAuMy1AeKbPHzeClZJziGGKlCZdyg4PsCleMjWjONWjfjN8gHyrBxcd1iFOey3
+HC7DYv/aDSqv4tCbJVWPvsNQrVyi93whrUdwwCR7cxbvtWV6RrrX8GD47QgWKmj
N1KSxSin8fCn1wgg15637/62A0NxN3XFGOF1ZSpRAkmB/R8toKDyHZnrlkK1poIq
OKfEuDdeR+SgOuspSIh7J1tEBirlVIGzcEcaZ3ACm52V71oDbgpaiKJi1QnrtqUZ
9VgrwH1dhDLtisowxcl/mbWNy+nQcWly5hzirE7cNHtkbY3bXpuWceMQuuCD2pF0
NCrMkt1ti7QkREesSDDJ6Hih8QIDAQABo1MwUTAdBgNVHQ4EFgQUX6LvUYIK1jmp
G5axSbBhXh2SUMgwHwYDVR0jBBgwFoAUX6LvUYIK1jmpG5axSbBhXh2SUMgwDwYD
VR0TAQH/BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOCAQEAO9qlS3mKHFwXu4RTbOxw
UjolTVCO9vJAuGPpnPk988hxtpHhoIKLFL30C/PPpT0wahh0GRzEhCBe4pZXisWE
3UGTHDN/KfWNTjGNj2PripFEN5cGmK/mtLv4RUQNYcEgi4/wtj8rt6rsRWA8zfv2
PAGGPVbC5hBHENeVg04l3bjWJYRdCP1E8/cegHMdsFChb/9ZpxFb2cGJDF27kG6Y
0+7HxlHz395Kwy4a27WooaHxcrS2x5ZX3hTgRa5hRic3TvT8HBM4s9F9Rdf/vCoX
9Gp+R5eNOzljdSk7gNF9cldlI2iUbHdEc4/ESwl8G/VqT7nf1aur4ZDtKyl88EkN
bg==
-----END CERTIFICATE-----
"#;

/// `openssl x509 -noout -fingerprint -sha1` of FIXTURE_PEM.
const FIXTURE_SHA1: &str = "FC78374057D7B930128238B5AC29E5531816611E";
/// Same, lowercase (the Windows `certutil` "Cert Hash(sha1)" style).
const FIXTURE_SHA1_LOWER: &str = "fc78374057d7b930128238b5ac29e5531816611e";
/// `openssl x509 -noout -fingerprint -sha1` of SECOND_PEM.
const SECOND_SHA1: &str = "645A81ADFB23686B247A6DFE1AB3CED12061811E";

const CA_PATH: &str = "/c/ca.pem";
const KEYCHAIN: &str = "/Users/u/Library/Keychains/login.keychain-db";

fn run_out(code: Option<i32>, stdout: &str, stderr: &str) -> RunOutput {
    RunOutput {
        code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

fn macos_store() -> TrustStore {
    TrustStore::MacOs {
        login_keychain: PathBuf::from(KEYCHAIN),
    }
}

// ---------------------------------------------------------------------------
// PeakbotCa::parse
// ---------------------------------------------------------------------------

#[test]
fn parse_ok_gives_path_and_sha1_thumbprint() {
    let ca = PeakbotCa::parse(Path::new(CA_PATH), FIXTURE_PEM).expect("fixture PEM parses");
    assert_eq!(ca.path(), CA_PATH, "path() is the path parse was given");
    assert_eq!(
        ca.thumbprint(),
        FIXTURE_SHA1,
        "thumbprint = SHA-1 of the cert's DER, 40 UPPERCASE hex"
    );
}

#[test]
fn parse_crlf_line_endings_same_thumbprint() {
    let pem = FIXTURE_PEM.replace('\n', "\r\n");
    let ca = PeakbotCa::parse(Path::new(CA_PATH), &pem).expect("CRLF PEM parses");
    assert_eq!(
        ca.thumbprint(),
        FIXTURE_SHA1,
        "CRLF must not change the thumbprint"
    );
}

#[test]
fn parse_leading_junk_before_begin_same_thumbprint() {
    let pem = format!("some junk text before the cert\n{FIXTURE_PEM}");
    let ca = PeakbotCa::parse(Path::new(CA_PATH), &pem).expect("junk before BEGIN parses");
    assert_eq!(
        ca.thumbprint(),
        FIXTURE_SHA1,
        "leading junk must be ignored"
    );
}

#[test]
fn parse_trailing_blank_lines_same_thumbprint() {
    let pem = format!("{FIXTURE_PEM}\n\n\n");
    let ca = PeakbotCa::parse(Path::new(CA_PATH), &pem).expect("trailing blanks parse");
    assert_eq!(
        ca.thumbprint(),
        FIXTURE_SHA1,
        "trailing blank lines must be ignored"
    );
}

#[test]
fn parse_two_certificate_blocks_uses_the_first() {
    let pem = format!("{FIXTURE_PEM}{SECOND_PEM}");
    let ca = PeakbotCa::parse(Path::new(CA_PATH), &pem).expect("two-block PEM parses");
    assert_eq!(
        ca.thumbprint(),
        FIXTURE_SHA1,
        "the FIRST block wins (not {SECOND_SHA1})"
    );
}

#[test]
fn parse_empty_is_err() {
    assert!(
        PeakbotCa::parse(Path::new(CA_PATH), "").is_err(),
        "empty input is an error"
    );
}

#[test]
fn parse_no_begin_is_err() {
    assert!(
        PeakbotCa::parse(Path::new(CA_PATH), "not a pem at all\njust text\n").is_err(),
        "no BEGIN CERTIFICATE is an error"
    );
}

#[test]
fn parse_begin_without_end_is_err() {
    let pem = "-----BEGIN CERTIFICATE-----\nMIIBFakeBody\n";
    assert!(
        PeakbotCa::parse(Path::new(CA_PATH), pem).is_err(),
        "BEGIN without END is an error"
    );
}

#[test]
fn parse_garbage_body_is_err() {
    let pem = "-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n";
    assert!(
        PeakbotCa::parse(Path::new(CA_PATH), pem).is_err(),
        "a non-base64 body is an error"
    );
}

#[cfg(unix)]
#[test]
fn parse_non_utf8_path_is_err() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(OsStr::from_bytes(b"/c/\xff.pem"));
    assert!(
        PeakbotCa::parse(path, FIXTURE_PEM).is_err(),
        "a non-UTF-8 path must be an error"
    );
}

// ---------------------------------------------------------------------------
// TrustStore::for_platform
// ---------------------------------------------------------------------------

#[test]
fn for_platform_linux_is_none() {
    assert_eq!(
        TrustStore::for_platform(Platform::Linux, Path::new("/home/u")),
        None,
        "Linux: the webview trusts the CA itself — no system store"
    );
}

#[test]
fn for_platform_windows_is_windows() {
    assert_eq!(
        TrustStore::for_platform(Platform::Windows, Path::new("C:\\Users\\u")),
        Some(TrustStore::Windows),
        "Windows: the user Root store"
    );
}

#[test]
fn for_platform_macos_is_login_keychain_under_home() {
    assert_eq!(
        TrustStore::for_platform(Platform::MacOs, Path::new("/Users/u")),
        Some(macos_store()),
        "macOS: the user's login keychain under the home dir"
    );
}

// ---------------------------------------------------------------------------
// check_cmd / add_cmd / Display
// ---------------------------------------------------------------------------

#[test]
fn macos_check_cmd_is_security_verify_cert() {
    let ca = PeakbotCa::parse(Path::new(CA_PATH), FIXTURE_PEM).expect("fixture parses");
    let cmd = macos_store().check_cmd(&ca);
    assert_eq!(cmd.program, "/usr/bin/security");
    assert_eq!(
        cmd.args,
        vec![
            "verify-cert".to_string(),
            "-c".to_string(),
            CA_PATH.to_string(),
            "-p".to_string(),
            "ssl".to_string(),
            "-l".to_string(),
            "-L".to_string(),
            "-R".to_string(),
            "offline".to_string(),
        ]
    );
}

#[test]
fn macos_add_cmd_is_security_add_trusted_cert() {
    let ca = PeakbotCa::parse(Path::new(CA_PATH), FIXTURE_PEM).expect("fixture parses");
    let cmd = macos_store().add_cmd(&ca);
    assert_eq!(cmd.program, "/usr/bin/security");
    assert_eq!(
        cmd.args,
        vec![
            "add-trusted-cert".to_string(),
            "-r".to_string(),
            "trustRoot".to_string(),
            "-p".to_string(),
            "ssl".to_string(),
            "-k".to_string(),
            KEYCHAIN.to_string(),
            CA_PATH.to_string(),
        ]
    );
}

#[test]
fn windows_check_cmd_is_certutil_store_lookup_by_thumbprint() {
    let ca = PeakbotCa::parse(Path::new(CA_PATH), FIXTURE_PEM).expect("fixture parses");
    let cmd = TrustStore::Windows.check_cmd(&ca);
    assert_eq!(cmd.program, "certutil.exe");
    assert_eq!(
        cmd.args,
        vec![
            "-user".to_string(),
            "-store".to_string(),
            "Root".to_string(),
            FIXTURE_SHA1.to_string(),
        ],
        "the lookup is by the CA's SHA-1 thumbprint"
    );
}

#[test]
fn windows_add_cmd_is_certutil_addstore() {
    let ca = PeakbotCa::parse(Path::new(CA_PATH), FIXTURE_PEM).expect("fixture parses");
    let cmd = TrustStore::Windows.add_cmd(&ca);
    assert_eq!(cmd.program, "certutil.exe");
    assert_eq!(
        cmd.args,
        vec![
            "-user".to_string(),
            "-addstore".to_string(),
            "Root".to_string(),
            CA_PATH.to_string(),
        ]
    );
}

#[test]
fn trust_cmd_display_is_program_and_args_joined() {
    let cmd = TrustCmd {
        program: "/usr/bin/security",
        args: vec![
            "add-trusted-cert".to_string(),
            "-r".to_string(),
            "trustRoot".to_string(),
            "-p".to_string(),
            "ssl".to_string(),
            "-k".to_string(),
            KEYCHAIN.to_string(),
            CA_PATH.to_string(),
        ],
    };
    assert_eq!(
        cmd.to_string(),
        "/usr/bin/security add-trusted-cert -r trustRoot -p ssl -k /Users/u/Library/Keychains/login.keychain-db /c/ca.pem"
    );
}

// ---------------------------------------------------------------------------
// is_trusted
// ---------------------------------------------------------------------------

#[test]
fn macos_is_trusted_purely_by_exit_code() {
    let ca = PeakbotCa::parse(Path::new(CA_PATH), FIXTURE_PEM).expect("fixture parses");
    let store = macos_store();
    assert!(
        store.is_trusted(&ca, &run_out(Some(0), "anything", "")),
        "exit 0 ⇒ trusted (stdout is irrelevant)"
    );
    assert!(
        !store.is_trusted(&ca, &run_out(Some(1), "anything", "")),
        "exit 1 ⇒ not trusted"
    );
    assert!(
        !store.is_trusted(&ca, &run_out(None, "anything", "")),
        "no exit code ⇒ not trusted"
    );
}

#[test]
fn windows_is_trusted_needs_exit_zero_and_thumbprint_in_stdout() {
    let ca = PeakbotCa::parse(Path::new(CA_PATH), FIXTURE_PEM).expect("fixture parses");
    let store = TrustStore::Windows;

    // certutil prints `Cert Hash(sha1): ab cd ef …` — spaced, lowercase.
    let spaced_lower: String = FIXTURE_SHA1_LOWER
        .as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    let out = format!("Cert Hash(sha1): {spaced_lower}\n");
    assert!(
        store.is_trusted(&ca, &run_out(Some(0), &out, "")),
        "exit 0 + spaced lowercase hash in stdout ⇒ trusted"
    );

    // Uppercase, contiguous.
    let out = format!("Hash: {FIXTURE_SHA1}\n");
    assert!(
        store.is_trusted(&ca, &run_out(Some(0), &out, "")),
        "exit 0 + uppercase contiguous hash ⇒ trusted"
    );

    // A different 40-hex thumbprint is NOT trusted.
    assert!(
        !store.is_trusted(&ca, &run_out(Some(0), SECOND_SHA1, "")),
        "a different thumbprint ⇒ not trusted"
    );

    // Non-zero exit beats a matching hash.
    assert!(
        !store.is_trusted(&ca, &run_out(Some(1), FIXTURE_SHA1, "")),
        "exit 1 ⇒ not trusted, even with the hash present"
    );

    // Exit 0 but the hash is missing from stdout.
    assert!(
        !store.is_trusted(&ca, &run_out(Some(0), "", "")),
        "exit 0 with empty stdout ⇒ not trusted"
    );
}
