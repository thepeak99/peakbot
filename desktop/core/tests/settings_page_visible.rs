//! Regression test: the bundled settings page must not render blank.
//!
//! THE BUG (diagnosed, fix lands later). Commit `a62b8b7` ("wip(desktop):
//! T16 in-window settings page, restart sentinel") wrapped the whole settings
//! form in a `<div id="page" hidden>` container (`desktop/ui/settings.html`)
//! and rewrote `desktop/ui/settings.js`. The rewrite polls `settings_get`
//! every 1000 ms and, on success, fills in the background row — but it never
//! removes the `hidden` attribute from `#page`. The `settings_get` IPC works;
//! the page simply stays hidden, so Settings → "Desktop app settings" shows
//! only the `Settings` heading on a blank page.
//!
//! THE CONTRACT (RED until the fix lands). The page is visible once EITHER:
//!
//!   1. `#page` is NOT `hidden` in the committed `desktop/ui/settings.html`
//!      (i.e. the container is never hidden in the first place), OR
//!
//!   2. `desktop/ui/settings.js` un-hides `#page` on the `settings_get`
//!      success path — sets `.hidden = false` or removes the `hidden`
//!      attribute from the `#page` element inside the `invoke("settings_get")`
//!      `.then(...)` callback.
//!
//! These tests read the COMMITTED `desktop/ui` files (no Tauri, no GUI, no
//! network, no wall-clock) so they run on every host as part of
//! `cargo test --manifest-path desktop/Cargo.toml --workspace`.
//!
//! They are written FIRST: today both halves are false (`#page` is hidden in
//! the HTML and `settings.js` never un-hides it), so the regression test
//! below is RED for exactly the blank-page reason.

use std::path::Path;

/// Read a committed file from `desktop/ui/` (a sibling of this crate's
/// `core/` dir). Panics with a setup explanation if the file is missing —
/// so a failure is never confused with the blank-page bug itself.
fn read_ui(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("ui").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "test setup: cannot read committed UI file {} ({e}): \
             desktop/ui must be committed in the repo",
            path.display()
        )
    })
}

/// Extract the opening tag that declares `id="page"` from the HTML, if any.
///
/// Searches for the exact `id="page"` / `id='page'` attribute (the closing
/// quote disambiguates it from e.g. `id="page-foo"`), then extends left to
/// the enclosing `<` and right to the tag's `>`.
fn page_opening_tag(html: &str) -> Option<String> {
    for id_attr in ["id=\"page\"", "id='page'"] {
        if let Some(pos) = html.find(id_attr) {
            let start = html[..pos].rfind('<')?;
            let end = pos + html[pos..].find('>')?;
            return Some(html[start..=end].to_string());
        }
    }
    None
}

/// True if the opening tag carries `hidden` as a standalone attribute token
/// (the boolean form; a `hidden="false"` value is not a token match and is
/// therefore NOT treated as hidden — which is correct per the HTML spec,
/// where any value on `hidden` would hide, but our page uses the bare form).
fn tag_has_hidden_attr(tag: &str) -> bool {
    let inner = tag.trim().trim_start_matches('<').trim_end_matches('>');
    inner.split(char::is_whitespace).any(|tok| tok == "hidden")
}

/// True if `settings.js` un-hides the `#page` element: it sets
/// `.hidden = false` on `#page`, or removes the `hidden` attribute from it.
///
/// All whitespace is stripped first, so spacing variations
/// (`$("page").hidden = false` vs `$("page").hidden=false`) normalize to one
/// form. Single and double quotes are both accepted, and the element may be
/// referenced via the page's `$` helper or `document.getElementById`.
fn js_unhides_page(js: &str) -> bool {
    let s: String = js.chars().filter(|c| !c.is_whitespace()).collect();
    const PATTERNS: &[&str] = &[
        // `.hidden = false` on #page
        "$(\"page\").hidden=false",
        "$('page').hidden=false",
        "getElementById(\"page\").hidden=false",
        "getElementById('page').hidden=false",
        // `removeAttribute("hidden")` on #page
        "$(\"page\").removeAttribute(\"hidden\")",
        "$('page').removeAttribute('hidden')",
        "getElementById(\"page\").removeAttribute(\"hidden\")",
        "getElementById('page').removeAttribute('hidden')",
    ];
    PATTERNS.iter().any(|p| s.contains(p))
}

// R1 — sanity guard (GREEN today): the committed UI files the regression test
// reads must exist and be non-empty. This keeps a missing-file setup problem
// from being mistaken for (or masking) the blank-page bug.
#[test]
fn settings_ui_files_are_committed_and_nonempty() {
    for name in ["settings.html", "settings.js"] {
        let content = read_ui(name);
        assert!(
            !content.trim().is_empty(),
            "desktop/ui/{name} must be committed and non-empty"
        );
    }
}

// R2 — THE REGRESSION (RED today): after a successful `settings_get` load the
// settings page must be visible. Either `#page` is not hidden in the HTML, or
// settings.js un-hides `#page` on the success path.
#[test]
fn settings_page_is_visible_after_successful_load() {
    let html = read_ui("settings.html");
    let js = read_ui("settings.js");

    let html_page_hidden = match page_opening_tag(&html) {
        Some(tag) => tag_has_hidden_attr(&tag),
        None => panic!(
            "test setup: settings.html declares no element with id=\"page\" \
             (the settings content container is missing)"
        ),
    };
    let js_unhides = js_unhides_page(&js);

    assert!(
        !html_page_hidden || js_unhides,
        "blank settings page: #page is hidden in settings.html and settings.js \
         never un-hides it after the settings_get load. Fix by removing the \
         `hidden` attribute from #page in settings.html, OR by un-hiding #page \
         (`.hidden = false` / removeAttribute) in the settings_get success path \
         of settings.js. (html #page hidden = {html_page_hidden}, js un-hides \
         #page = {js_unhides})"
    );
}
