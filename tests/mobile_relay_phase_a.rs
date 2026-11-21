//! `Catalog::home()` tests for remotes through the on-device core.
//!
//! API under test:
//! ```ignore
//! // peakbot::config::resolve
//! pub enum Home {
//!     Local(Bound),
//!     Remote(Arc<RemoteProfile>),
//! }
//! impl Catalog {
//!     /// The session's home: the first LOCAL profile, else the first
//!     /// remote (file order) — a total function, never a panic.
//!     pub fn home(&self) -> Home;
//!     /// The first local profile's first agent; `None` when the catalog
//!     /// has no local profile (relay-only).
//!     pub fn default_bound(&self) -> Option<Bound>;
//! }
//! ```

use std::path::Path;
use std::sync::Arc;

use peakbot::config::remote::RemoteProfile;
use peakbot::config::resolve::{Bound, Config, Home, ParseCtx, Source};

/// A relay-only config: one remote profile, no providers, no local profile.
const RELAY_ONLY: &str = "profiles:\n  dev on nas:\n    remote:\n      url: https://nas.lan:7823\n      token: tok123\n      ca: sha256:3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab\n      profile: dev\n";

/// A mixed config: the remote is listed FIRST, then a local profile.
const REMOTE_FIRST_THEN_LOCAL: &str = "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }\nprofiles:\n  dev on nas:\n    remote:\n      url: https://nas.lan:7823\n      token: tok123\n      ca: sha256:3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab\n      profile: dev\n  dev:\n    agents:\n      team:\n        model: sonnet\n";

fn parse(yaml: &str) -> Config {
    Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text: yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect("fixture config must parse")
}

/// The home is the first LOCAL profile even when a remote is listed
/// first — a remote entry never becomes the home of a local session.
#[test]
fn home_is_first_local_even_when_a_remote_is_listed_first() {
    let home = parse(REMOTE_FIRST_THEN_LOCAL).catalog.home();
    match home {
        Some(Home::Local(bound)) => {
            assert_eq!(bound.profile.name, "dev", "the local profile is the home");
            assert_eq!(
                bound.agent().main.name,
                "team",
                "the home binds the local profile's first agent"
            );
        }
        Some(Home::Remote(remote)) => {
            panic!(
                "a catalog with a local profile homes local, got remote '{}'",
                remote.name
            )
        }
        None => panic!("catalog with local profile should have a home"),
    }
}

/// A relay-only config homes the FIRST remote (file order) — the
/// phone with no local profile relays there on every unpinned attach.
#[test]
fn relay_only_home_is_the_first_remote() {
    let home = parse(RELAY_ONLY).catalog.home();
    match home {
        Some(Home::Local(bound)) => panic!(
            "a relay-only catalog homes a remote, got local '{}'",
            bound.profile.name
        ),
        Some(Home::Remote(remote)) => {
            assert_eq!(remote.name, "dev on nas", "the first remote is the home");
        }
        None => panic!("relay-only catalog should have a remote home"),
    }
}

/// `default_bound` becomes `Option<Bound>` — `None` for a relay-only
/// catalog (there is nothing to bind locally), checked once at each
/// boundary instead of `expect`ed downstream.
#[test]
fn default_bound_is_none_for_relay_only() {
    let bound: Option<Bound> = parse(RELAY_ONLY).catalog.default_bound();
    assert!(bound.is_none(), "no local profile ⇒ no default bound");
}

/// With a local profile present, `default_bound` is `Some` of the
/// first local's first agent — the remote-first ordering doesn't change it.
#[test]
fn default_bound_is_first_local_when_a_remote_is_listed_first() {
    let bound: Option<Bound> = parse(REMOTE_FIRST_THEN_LOCAL).catalog.default_bound();
    let bound = bound.expect("a local profile ⇒ a default bound");
    assert_eq!(bound.profile.name, "dev");
    assert_eq!(bound.agent().main.name, "team");
}

/// Type-shape lock: `Home::Remote` carries the `Arc<RemoteProfile>`
/// (the builder needs the endpoint/token target, not just a name).
#[test]
fn home_remote_carries_the_remote_profile() {
    let home = parse(RELAY_ONLY).catalog.home();
    let Some(Home::Remote(remote)) = home else {
        panic!("relay-only catalog homes the remote");
    };
    let _name: &String = &remote.name;
    let _arc: Arc<RemoteProfile> = Arc::clone(&remote);
}
