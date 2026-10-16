//! Trust of PeakBot's self-signed CA in the Linux webview (#376, design
//! #377b §4.5). One `load-failed-with-tls-errors` handler on the main
//! webview: when the load of OUR current backend origin (https,
//! 127.0.0.1) fails on TLS and the presented certificate is
//! cryptographically signed by `~/.cache/peakbot/tls/ca-cert.pem`, it
//! allows the certificate for the host and reloads the URL on the next
//! main-loop idle. Everything else falls through to WebKit's normal
//! error page.
//!
//! The trust decision is made by the CA signature on the leaf the
//! server actually presents — never by a file describing what the
//! server *will* present (T-B1): stale leaves, restarts and re-mints
//! are all covered by the one failing handshake.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, RwLock};

use gio::prelude::*;
use tauri::WebviewWindow;
use webkit2gtk::{WebContextExt, WebViewExt};

use peakbot_desktop_core::nav::may_trust_peakbot_cert;
use peakbot_desktop_core::paths;

/// Connect the TLS trust handler to the main webview (called once,
/// right after the main window is built). Logs and continues when
/// `with_webview` fails (degrades to WebKit's documented error page).
pub fn install(win: &WebviewWindow, nav: Arc<RwLock<Option<String>>>) {
    let result = win.with_webview(move |pv| {
        let wv = pv.inner();
        // The certificate allowed on the previous failure: a second
        // failure with the SAME certificate means the allowance did
        // not take — give up and show the error page (loop guard).
        let last: Rc<RefCell<Option<gio::TlsCertificate>>> = Default::default();
        wv.connect_load_failed_with_tls_errors(move |wv, uri, cert, _flags| {
            let Ok(url) = tauri::Url::parse(uri) else {
                return false;
            };
            let origin = nav.read().expect("nav poisoned").clone();
            if !may_trust_peakbot_cert(&url, origin.as_deref()) {
                return false; // not our backend origin: WebKit's error page
            }
            if last.borrow().as_ref().is_some_and(|c| c.is_same(cert)) {
                log::warn!(
                    "backend TLS certificate was still rejected after being trusted; showing the error page"
                );
                return false;
            }
            match signed_by_peakbot_ca(cert) {
                Ok(true) => {}
                Ok(false) => {
                    log::warn!("backend TLS certificate is not signed by the PeakBot CA");
                    return false;
                }
                Err(e) => {
                    // The reason only — `uri` carries ?token=.
                    log::warn!("cannot verify the backend TLS certificate: {e}");
                    return false;
                }
            }
            let Some(ctx) = wv.context() else {
                log::warn!("no web context to allow the TLS certificate");
                return false;
            };
            ctx.allow_tls_certificate_for_host(cert, "127.0.0.1");
            *last.borrow_mut() = Some(cert.clone());
            // Reload on the next idle tick, not re-entrantly inside the
            // signal (a provisional failure leaves the status page
            // committed, so `reload()` would reload the wrong page) —
            // re-checking that the backend origin is still the same.
            let wv = wv.clone();
            let uri = uri.to_string();
            let nav = nav.clone();
            gio::glib::idle_add_local_once(move || {
                if nav.read().expect("nav poisoned").clone() == origin {
                    wv.load_uri(&uri);
                }
            });
            true // handled ⇒ no load-failed, no error page
        });
    });
    if let Err(e) = result {
        log::warn!("TLS trust handler not attached: {e}");
    }
}

/// Signature check (T-B2): the presented leaf must verify against the
/// PeakBot CA with NO flags — an issuer-DN compare would be forgeable.
/// `identity = None`: the host is already pinned to `127.0.0.1` by
/// `may_trust_peakbot_cert`, and GLib's IP-identity matching varies
/// across glib-networking versions.
fn signed_by_peakbot_ca(cert: &gio::TlsCertificate) -> Result<bool, String> {
    let Some(ca_path) = paths::peakbot_ca_cert() else {
        return Err("no cache directory for the PeakBot CA".into());
    };
    // Read lazily: the CA may be born after the app started.
    let pem = std::fs::read_to_string(&ca_path).map_err(|e| e.to_string())?;
    let ca = gio::TlsCertificate::from_pem(&pem).map_err(|e| e.to_string())?;
    Ok(cert
        .verify(None::<&gio::SocketConnectable>, Some(&ca))
        .is_empty())
}
