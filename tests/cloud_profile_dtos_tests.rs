//! Regression tests for the `/api/cloud/devices/{id}/profiles` response shaper.
//!
//! Confirmed bug: a phone linked to two PCs that both ship profile `default`
//! saw one host. The peers' `/profiles` rows carried their own peer-chosen
//! colour strings, so the SPA could not colour rows per host. The server now
//! shapes the response through a small pure helper:
//!
//! ```ignore
//! pub fn profile_dtos(
//!     profiles: &[crate::cloud::session::ProfileInfo],
//!     slug: Option<&str>,
//! ) -> Vec<serde_json::Value>
//! ```
//!
//! Contract:
//!   * every row's `color` is replaced with `TabColor::derived(host slug)` —
//!     never the peer's arbitrary colour string;
//!   * an unknown slug (the device is no longer in the active list) OMITS the
//!     `color` key entirely — never `"color": null`;
//!   * `name` and every other field pass through verbatim.
//!
//! RED before implementation: `peakbot::ui::web::cloud_api::profile_dtos`
//! does not exist yet, so this whole test binary fails to compile (feature
//! absent). The developer is expected to add the helper with this signature
//! (or adapt the test if they choose an equivalent shape — see the report).

use peakbot::cloud::session::ProfileInfo;
use peakbot::config::file::TabColor;
use peakbot::ui::web::cloud_api::profile_dtos;
use serde_json::json;

fn peer(name: &str, color: &str) -> ProfileInfo {
    ProfileInfo {
        name: name.to_string(),
        color: color.to_string(),
    }
}

#[test]
fn overwrites_every_peers_color_with_the_host_derived_color() {
    let rows = profile_dtos(
        &[peer("default", "peer-red"), peer("work", "peer-green")],
        Some("phone"),
    );

    assert_eq!(rows.len(), 2, "every peer row survives the shaping");
    let want = json!(TabColor::derived("phone"));
    for row in &rows {
        assert_eq!(
            row.get("color"),
            Some(&want),
            "the peer's own colour string must be replaced by the host colour"
        );
    }
}

#[test]
fn keeps_the_profile_name_verbatim() {
    let rows = profile_dtos(&[peer("default", "peer-red")], Some("desk"));
    assert_eq!(
        rows[0]["name"], "default",
        "the profile name is not rewritten"
    );
}

#[test]
fn omits_the_color_key_entirely_when_the_slug_is_unknown() {
    let rows = profile_dtos(&[peer("default", "peer-red")], None);

    let obj = rows[0]
        .as_object()
        .expect("each shaped row is a JSON object");
    assert!(
        !obj.contains_key("color"),
        "unknown host ⇒ no color key at all (never null): {obj:?}"
    );

    // And the serialized body must not carry `"color": null` either.
    let wire = serde_json::to_string(&rows).expect("rows serialize");
    assert!(
        !wire.contains("\"color\""),
        "unknown host must omit color, got: {wire}"
    );
}

#[test]
fn same_host_colors_every_profile_alike() {
    let rows = profile_dtos(
        &[peer("alpha", "peer-a"), peer("beta", "peer-b")],
        Some("desk"),
    );
    assert_eq!(rows[0]["color"], rows[1]["color"]);
    assert_eq!(rows[0]["color"], json!(TabColor::derived("desk")));
}

#[test]
fn returns_an_empty_list_for_an_empty_input() {
    assert!(profile_dtos(&[], Some("desk")).is_empty());
    assert!(profile_dtos(&[], None).is_empty());
}
