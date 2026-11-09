//! iOS glue: the WKWebView edge-swipe back
//! gesture and the data root's exclusion from iCloud backups. Compiled for
//! the iOS target only — the sim build (`ios-sim.sh`) is the compile check.

/// Tauri 2.12 has no builder option for WKWebView's edge-swipe
/// back/forward gestures (wry has the attribute, Tauri doesn't plumb it), so
/// set the property natively. Off by default in WKWebView.
pub fn enable_back_gestures(w: &tauri::WebviewWindow) {
    let res = w.with_webview(|pw| unsafe {
        use objc2::{msg_send, runtime::AnyObject};
        let wk: &AnyObject = &*(pw.inner() as *mut AnyObject);
        let _: () = msg_send![wk, setAllowsBackForwardNavigationGestures: true];
        let enabled: bool = msg_send![wk, allowsBackForwardNavigationGestures];
        tauri_plugin_log::log::info!("edge-swipe back enabled: {enabled}");
    });
    if let Err(e) = res {
        tauri_plugin_log::log::warn!("back gestures not enabled: {e}");
    }
}

/// The data root `D` holds pairing tokens and the Local core's config, so it
/// must not ride along in an iCloud backup: set the
/// `NSURLIsExcludedFromBackupKey` resource value on it at boot.
pub fn exclude_data_root_from_backup(data_root: &std::path::Path) {
    use objc2_foundation::{NSNumber, NSString, NSURL, NSURLIsExcludedFromBackupKey};

    let Some(path) = data_root.to_str() else {
        tauri_plugin_log::log::warn!("backup exclusion: non-UTF-8 data root");
        return;
    };
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    let value = NSNumber::numberWithBool(true);
    match unsafe { url.setResourceValue_forKey_error(Some(&value), NSURLIsExcludedFromBackupKey) } {
        Ok(()) => {
            tauri_plugin_log::log::info!("data root excluded from iCloud backup: {path}")
        }
        Err(e) => tauri_plugin_log::log::warn!("backup exclusion failed for {path}: {e}"),
    }
}
