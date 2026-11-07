//! `Profiles` — the app's only persisted state (remotes, `last`, Local consent)
//! and its `profiles.json`. The mutators are the only writers, so
//! P1–P3 hold by construction.

use serde::{Deserialize, Serialize};
use serde::{
    de::{self, Deserializer},
    ser::{SerializeStruct, Serializer},
};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use crate::link::{LinkError, ServerLink};

/// A stable, monotonically increasing, never-reused id for a remote profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoteId(pub u32);

impl RemoteId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }
    pub fn get(self) -> u32 {
        self.0
    }
}

/// A remote's display name: trimmed, 1..=40 chars; defaults to `link.host_label()`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileName(String);

impl ProfileName {
    /// Trim + validate; the default name is always a valid host label.
    pub fn new(raw: &str) -> Result<Self, ProfileError> {
        let trimmed = raw.trim();
        let chars = trimmed.chars().count();
        if !(1..=40).contains(&chars) {
            return Err(ProfileError::BadName);
        }
        Ok(Self(trimmed.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Errors from editing the store. `Display` is what the Profiles page shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProfileError {
    DuplicateServer,
    BadName,
    NoSuchProfile,
    /// The pasted/scanned text isn't a pairing link (adding a profile).
    Link(LinkError),
}

impl std::error::Error for ProfileError {}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileError::DuplicateServer => f.write_str("That server is already in your list."),
            ProfileError::BadName => f.write_str("Names need 1 to 40 characters."),
            ProfileError::NoSuchProfile => f.write_str("That profile no longer exists."),
            ProfileError::Link(e) => e.fmt(f),
        }
    }
}

impl From<LinkError> for ProfileError {
    fn from(e: LinkError) -> Self {
        ProfileError::Link(e)
    }
}

/// Which profile is active: the Local edition, or a named remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Active {
    Local,
    Remote(RemoteId),
}

impl Serialize for Active {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Active::Local => serializer.serialize_str("local"),
            Active::Remote(id) => {
                let mut s = serializer.serialize_struct("Remote", 1)?;
                s.serialize_field("remote", &id.get())?;
                s.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Active {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v: serde_json::Value = serde_json::Value::deserialize(deserializer)?;
        Ok(match v {
            serde_json::Value::String(_) => Active::Local,
            serde_json::Value::Object(m) => match m.get("remote").and_then(|n| n.as_u64()) {
                Some(n) => Active::Remote(RemoteId::new(n as u32)),
                None => Active::Local,
            },
            _ => Active::Local,
        })
    }
}

/// One paired remote server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    pub id: RemoteId,
    pub name: ProfileName,
    pub link: ServerLink,
}

impl Serialize for ServerLink {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.url().as_str())
    }
}

impl<'de> Deserialize<'de> for ServerLink {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw: String = String::deserialize(deserializer)?;
        ServerLink::parse(&raw).map_err(de::Error::custom)
    }
}

/// The store of paired remotes + active choice, persisted as `profiles.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profiles {
    remotes: Vec<Remote>,
    last: Active,
    next_id: u32,
    local_consent: bool,
}

impl Profiles {
    /// Load the store, or the empty default on missing/corrupt/bad-link input
    /// (P2 dangling `last` also resolves to Local).
    pub fn load(path: &Path) -> Profiles {
        let raw = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Self::default(),
        };
        let parsed: Profiles = match serde_json::from_str(&raw) {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };
        let mut parsed = parsed;
        if let Active::Remote(id) = parsed.last {
            // dangling `last` id (no matching remote) → Local (P2).
            if parsed.remotes.iter().find(|r| r.id == id).is_none() {
                parsed.last = Active::Local;
            }
        }
        parsed
    }

    /// Atomic tmp + rename in the target dir so a crash never leaves a half file
    /// and no temp lingers.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let dir = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "save: path has no parent dir")
        })?;
        let bytes = serde_json::to_string(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let base = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let tmp = dir.join(format!(".{}.tmp", base));
        {
            let mut f = File::create(&tmp)?;
            f.write_all(bytes.as_bytes())?;
            f.flush()?;
        }
        fs::rename(&tmp, path)
    }

    pub fn remotes(&self) -> &[Remote] {
        &self.remotes
    }
    pub fn last(&self) -> &Active {
        &self.last
    }
    pub fn local_consent(&self) -> bool {
        self.local_consent
    }
    pub fn get(&self, id: RemoteId) -> Option<&Remote> {
        self.remotes.iter().find(|r| r.id == id)
    }

    /// Add a remote; P3 enforces one profile per origin, and the default name
    /// is `host_label()`.
    pub fn add(
        &mut self,
        link: ServerLink,
        name: Option<ProfileName>,
    ) -> Result<RemoteId, ProfileError> {
        if self
            .remotes
            .iter()
            .any(|r| r.link.origin() == link.origin())
        {
            return Err(ProfileError::DuplicateServer);
        }
        let name = match name {
            Some(n) => n,
            None => ProfileName::new(&link.host_label()).map_err(|_| ProfileError::BadName)?,
        };
        let id = RemoteId::new(self.next_id);
        self.next_id += 1;
        self.remotes.push(Remote { id, name, link });
        Ok(id)
    }

    pub fn rename(&mut self, id: RemoteId, name: ProfileName) -> Result<(), ProfileError> {
        if let Some(r) = self.remotes.iter_mut().find(|r| r.id == id) {
            r.name = name;
            Ok(())
        } else {
            Err(ProfileError::NoSuchProfile)
        }
    }

    /// Drop a remote; if it was active, P2 falls back to Local.
    pub fn remove(&mut self, id: RemoteId) -> Result<(), ProfileError> {
        let pos = self
            .remotes
            .iter()
            .position(|r| r.id == id)
            .ok_or(ProfileError::NoSuchProfile)?;
        if self.last == Active::Remote(id) {
            self.last = Active::Local;
        }
        self.remotes.remove(pos);
        Ok(())
    }

    pub fn set_last(&mut self, last: Active) {
        self.last = last;
    }
    pub fn set_local_consent(&mut self, consent: bool) {
        self.local_consent = consent;
    }
}

impl Default for Profiles {
    fn default() -> Self {
        Self {
            remotes: Vec::new(),
            last: Active::Local,
            next_id: 1,
            local_consent: false,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── test scaffolding: one fresh temp dir per test (independent) ──────

    static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new() -> Self {
            let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir()
                .join(format!("peakbot-mobile-test-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&p).expect("create temp dir");
            TmpDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn srv(text: &str) -> ServerLink {
        ServerLink::parse(text).unwrap_or_else(|e| panic!("test link {text:?} must parse: {e:?}"))
    }

    const TOKEN_A: &str = "aaaa0000aaaa0000aaaa0000aaaa0000";
    const TOKEN_B: &str = "bbbb1111bbbb1111bbbb1111bbbb1111";
    const TOKEN_C: &str = "cccc2222cccc2222cccc2222cccc2222";

    // ── default + corrupt-file robustness ────────────────────────────────

    #[test]
    fn default_store_is_empty_local_unconsented() {
        let store = Profiles::default();
        assert!(store.remotes().is_empty());
        assert_eq!(store.last(), &Active::Local);
        assert_eq!(*store.last(), Active::Local);
        assert_eq!(store.next_id, 1);
        assert!(!store.local_consent());
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = TmpDir::new();
        let store = Profiles::load(&dir.path().join("profiles.json"));
        assert!(store.remotes().is_empty());
        assert_eq!(store.last(), &Active::Local);
        assert_eq!(store.next_id, 1);
        assert!(!store.local_consent());
    }

    #[test]
    fn load_corrupt_json_returns_default() {
        let dir = TmpDir::new();
        let p = dir.path().join("profiles.json");
        std::fs::write(&p, r#"{"remotes": [broken"#).unwrap();
        let store = Profiles::load(&p);
        assert!(store.remotes().is_empty());
        assert_eq!(store.last(), &Active::Local);
        assert_eq!(store.next_id, 1);
        assert!(!store.local_consent());
    }

    #[test]
    fn load_garbage_returns_default() {
        let dir = TmpDir::new();
        let p = dir.path().join("profiles.json");
        std::fs::write(&p, "definitely not json").unwrap();
        let store = Profiles::load(&p);
        assert!(store.remotes().is_empty());
        assert_eq!(store.last(), &Active::Local);
        assert_eq!(store.next_id, 1);
    }

    #[test]
    fn load_empty_file_returns_default() {
        let dir = TmpDir::new();
        let p = dir.path().join("profiles.json");
        std::fs::write(&p, "").unwrap();
        let store = Profiles::load(&p);
        assert!(store.remotes().is_empty());
        assert_eq!(store.last(), &Active::Local);
        assert_eq!(store.next_id, 1);
    }

    // ── P1: exactly one Local, implicit ──────────────────────────────────

    #[test]
    fn p1_local_is_implicit_and_not_a_remote() {
        let mut store = Profiles::default();
        // Local exists (as `last`) without an entry in `remotes`.
        assert_eq!(store.last(), &Active::Local);
        assert!(
            store.remotes().is_empty(),
            "Local must not be a Remote entry (P1)"
        );

        store
            .add(srv(&format!("https://a.lan:7823/?token={TOKEN_A}")), None)
            .unwrap();
        store
            .add(srv(&format!("https://b.lan:7823/?token={TOKEN_B}")), None)
            .unwrap();
        assert_eq!(
            store.remotes().len(),
            2,
            "exactly the two added remotes — no Local row (P1)"
        );
        // every remote is a real server link; none of them is the Local profile
        for r in store.remotes() {
            assert_eq!(r.link.url().scheme(), "https");
        }

        // Local can be (re)selected as last while the remotes stay put
        store.set_last(Active::Local);
        assert_eq!(store.last(), &Active::Local);
        assert_eq!(store.remotes().len(), 2);
    }

    // ── P2: `last` names Local or an existing remote ─────────────────────

    #[test]
    fn removing_last_sets_local() {
        let mut store = Profiles::default();
        let a = store
            .add(srv(&format!("https://a.lan:7823/?token={TOKEN_A}")), None)
            .unwrap();
        store.set_last(Active::Remote(a));
        assert_eq!(store.last(), &Active::Remote(a));

        store.remove(a).unwrap();
        assert_eq!(
            store.last(),
            &Active::Local,
            "removing the `last` remote resets it to Local (P2)"
        );
    }

    #[test]
    fn dangling_last_parses_as_local() {
        let dir = TmpDir::new();
        let p = dir.path().join("profiles.json");
        std::fs::write(
            &p,
            r#"{"remotes":[],"last":{"remote":99},"next_id":1,"local_consent":false}"#,
        )
        .unwrap();
        let store = Profiles::load(&p);
        assert_eq!(
            store.last(),
            &Active::Local,
            "a dangling last id must parse as Local (P2)"
        );
    }

    // ── P3: remote origins are unique ────────────────────────────────────

    #[test]
    fn add_duplicate_origin_same_token_rejected() {
        let mut store = Profiles::default();
        store
            .add(srv(&format!("https://h:7823/?token={TOKEN_A}")), None)
            .unwrap();
        match store.add(srv(&format!("https://h:7823/?token={TOKEN_A}")), None) {
            Err(ProfileError::DuplicateServer) => {}
            other => panic!("expected DuplicateServer, got {other:?}"),
        }
    }

    #[test]
    fn add_same_origin_different_token_rejected() {
        let mut store = Profiles::default();
        store
            .add(srv(&format!("https://h:7823/?token={TOKEN_A}")), None)
            .unwrap();
        match store.add(srv(&format!("https://h:7823/?token={TOKEN_B}")), None) {
            Err(ProfileError::DuplicateServer) => {}
            other => panic!("expected DuplicateServer (origin, not link), got {other:?}"),
        }
    }

    #[test]
    fn add_different_port_is_not_duplicate() {
        let mut store = Profiles::default();
        store
            .add(srv(&format!("https://h:7823/?token={TOKEN_A}")), None)
            .unwrap();
        // a different port is a different origin
        store
            .add(srv(&format!("https://h:7824/?token={TOKEN_B}")), None)
            .unwrap();
        assert_eq!(store.remotes().len(), 2);
    }

    #[test]
    fn add_different_host_is_not_duplicate() {
        let mut store = Profiles::default();
        store
            .add(srv(&format!("https://a.lan:7823/?token={TOKEN_A}")), None)
            .unwrap();
        store
            .add(srv(&format!("https://b.lan:7823/?token={TOKEN_B}")), None)
            .unwrap();
        assert_eq!(store.remotes().len(), 2);
    }

    // ── ids: monotonic, never reused ─────────────────────────────────────

    #[test]
    fn add_assigns_monotonic_ids_from_one() {
        let mut store = Profiles::default();
        let a = store
            .add(srv(&format!("https://a.lan:7823/?token={TOKEN_A}")), None)
            .unwrap();
        let b = store
            .add(srv(&format!("https://b.lan:7823/?token={TOKEN_B}")), None)
            .unwrap();
        assert_eq!(a, RemoteId::new(1));
        assert_eq!(b, RemoteId::new(2));
        assert_eq!(store.next_id, 3);
    }

    #[test]
    fn ids_never_reused_after_remove() {
        let mut store = Profiles::default();
        let a = store
            .add(srv(&format!("https://a.lan:7823/?token={TOKEN_A}")), None)
            .unwrap();
        store
            .add(srv(&format!("https://b.lan:7823/?token={TOKEN_B}")), None)
            .unwrap();
        store.remove(a).unwrap();

        let c = store
            .add(srv(&format!("https://c.lan:7823/?token={TOKEN_C}")), None)
            .unwrap();
        assert_ne!(c, a, "ids are monotonic — never reused after remove");
        assert_eq!(c, RemoteId::new(3));
        assert_eq!(store.next_id, 4);
    }

    // ── names ────────────────────────────────────────────────────────────

    #[test]
    fn profile_name_rejects_empty() {
        match ProfileName::new("") {
            Err(ProfileError::BadName) => {}
            other => panic!("expected BadName for empty, got {other:?}"),
        }
    }

    #[test]
    fn profile_name_rejects_whitespace_only() {
        match ProfileName::new("   \t  ") {
            Err(ProfileError::BadName) => {}
            other => panic!("expected BadName for whitespace, got {other:?}"),
        }
    }

    #[test]
    fn profile_name_rejects_41_chars() {
        match ProfileName::new(&"x".repeat(41)) {
            Err(ProfileError::BadName) => {}
            other => panic!("expected BadName for 41 chars, got {other:?}"),
        }
    }

    #[test]
    fn profile_name_accepts_40_chars() {
        let name = ProfileName::new(&"x".repeat(40)).unwrap();
        assert_eq!(name.as_str().len(), 40);
    }

    #[test]
    fn profile_name_trims_surrounding_whitespace() {
        let name = ProfileName::new("  Home server  ").unwrap();
        assert_eq!(name.as_str(), "Home server");
    }

    #[test]
    fn profile_name_accepts_unicode() {
        let name = ProfileName::new("  Server 🏠  ").unwrap();
        assert_eq!(name.as_str(), "Server 🏠");
    }

    #[test]
    fn add_defaults_name_to_host_label() {
        let mut store = Profiles::default();
        let id = store
            .add(srv(&format!("https://nas.lan:7823/?token={TOKEN_A}")), None)
            .unwrap();
        assert_eq!(
            store.get(id).unwrap().name.as_str(),
            "nas.lan:7823",
            "no name given ⇒ default = link.host_label()"
        );
    }

    #[test]
    fn rename_updates_name() {
        let mut store = Profiles::default();
        let id = store
            .add(srv(&format!("https://a.lan:7823/?token={TOKEN_A}")), None)
            .unwrap();
        store
            .rename(id, ProfileName::new("New name").unwrap())
            .unwrap();
        assert_eq!(store.get(id).unwrap().name.as_str(), "New name");
    }

    #[test]
    fn rename_unknown_id_is_no_such_profile() {
        let mut store = Profiles::default();
        match store.rename(RemoteId::new(99), ProfileName::new("x").unwrap()) {
            Err(ProfileError::NoSuchProfile) => {}
            other => panic!("expected NoSuchProfile, got {other:?}"),
        }
    }

    #[test]
    fn remove_unknown_id_is_no_such_profile() {
        let mut store = Profiles::default();
        match store.remove(RemoteId::new(99)) {
            Err(ProfileError::NoSuchProfile) => {}
            other => panic!("expected NoSuchProfile, got {other:?}"),
        }
    }

    // ── P4: the token lives only in ServerLink::url() and profiles.json ──

    #[test]
    fn p4_token_appears_only_in_link_url_and_the_file() {
        let mut store = Profiles::default();
        store
            .add(
                srv(&format!("https://a.lan:7823/?token={TOKEN_A}")),
                Some(ProfileName::new("Home server").unwrap()),
            )
            .unwrap();

        // never in the store's Debug (logs, panics) …
        let dbg = format!("{store:?}");
        assert!(
            !dbg.contains(TOKEN_A),
            "Debug of the store leaked the token (P4): {dbg}"
        );
        // … never in any name …
        for r in store.remotes() {
            assert!(
                !r.name.as_str().contains(TOKEN_A),
                "name leaked the token (P4)"
            );
        }
        // … only in the sanctioned carriers: url() and the file
        assert!(store.remotes()[0].link.url().as_str().contains(TOKEN_A));

        let dir = TmpDir::new();
        let p = dir.path().join("profiles.json");
        store.save(&p).unwrap();
        let file = std::fs::read_to_string(&p).unwrap();
        assert!(
            file.contains(TOKEN_A),
            "profiles.json is the sanctioned at-rest home (P4)"
        );
    }

    // ── P5: Local's config is NOT in this model ──────────────────────────

    #[test]
    fn p5_stored_json_has_only_the_locked_fields() {
        let mut store = Profiles::default();
        store
            .add(srv(&format!("https://a.lan:7823/?token={TOKEN_A}")), None)
            .unwrap();
        store.set_local_consent(true);

        let dir = TmpDir::new();
        let p = dir.path().join("profiles.json");
        store.save(&p).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();

        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&raw).expect("profiles.json must be a JSON object");
        let keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["last", "local_consent", "next_id", "remotes"],
            "the file holds ONLY the locked fields — no Local provider/model/key/tools (P5)"
        );
        assert!(!raw.contains("api_key"), "no api_key in profiles.json (P5)");
        assert!(
            !raw.contains("provider"),
            "no provider in profiles.json (P5)"
        );
    }

    // ── the example JSON, both directions ───────────────────────────

    const EXAMPLE_TOKEN: &str = "deadbeefdeadbeefdeadbeefdeadbeef";

    #[test]
    fn json_round_trip_of_the_23_example() {
        let dir = TmpDir::new();
        let p = dir.path().join("profiles.json");
        // The exact example shape (`…` = a real token here).
        std::fs::write(
            &p,
            format!(
                r#"{{"remotes":[{{"id":1,"name":"Home server","link":"https://nas.lan:7823/?token={EXAMPLE_TOKEN}"}}],
                     "last":{{"remote":1}},"next_id":2,"local_consent":true}}"#
            ),
        )
        .unwrap();

        let store = Profiles::load(&p);
        assert_eq!(store.remotes().len(), 1);
        let r = &store.remotes()[0];
        assert_eq!(r.id, RemoteId::new(1));
        assert_eq!(r.name.as_str(), "Home server");
        assert_eq!(
            r.link.url().as_str(),
            &format!("https://nas.lan:7823/?token={EXAMPLE_TOKEN}")
        );
        assert_eq!(store.last(), &Active::Remote(RemoteId::new(1)));
        assert_eq!(store.next_id, 2);
        assert!(store.local_consent());
    }

    #[test]
    fn save_writes_the_23_example_json_exactly() {
        let mut store = Profiles::default();
        store
            .add(
                srv(&format!("https://nas.lan:7823/?token={EXAMPLE_TOKEN}")),
                Some(ProfileName::new("Home server").unwrap()),
            )
            .unwrap();
        store.set_last(Active::Remote(RemoteId::new(1)));
        store.set_local_consent(true);

        let dir = TmpDir::new();
        let p = dir.path().join("profiles.json");
        store.save(&p).unwrap();

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let expected: serde_json::Value = serde_json::from_str(&format!(
            r#"{{
              "remotes": [{{"id":1,"name":"Home server","link":"https://nas.lan:7823/?token={EXAMPLE_TOKEN}"}}],
              "last": {{"remote":1}},
              "next_id": 2,
              "local_consent": true
            }}"#
        ))
        .unwrap();
        assert_eq!(value, expected, "the wire format is the example, exactly");
    }

    // ── persistence: tmp + rename ────────────────────────────────────────

    #[test]
    fn save_then_load_round_trips() {
        let mut store = Profiles::default();
        let a = store
            .add(
                srv(&format!("https://a.lan:7823/?token={TOKEN_A}")),
                Some(ProfileName::new("Home server").unwrap()),
            )
            .unwrap();
        store
            .add(srv(&format!("https://b.lan:9443/?token={TOKEN_B}")), None)
            .unwrap();
        store.set_last(Active::Remote(a));
        store.set_local_consent(true);

        let dir = TmpDir::new();
        let p = dir.path().join("profiles.json");
        store.save(&p).unwrap();

        let reloaded = Profiles::load(&p);
        assert_eq!(reloaded.remotes().len(), 2);
        assert_eq!(reloaded.remotes()[0].id, store.remotes()[0].id);
        assert_eq!(reloaded.remotes()[0].name.as_str(), "Home server");
        assert_eq!(
            reloaded.remotes()[0].link.url().as_str(),
            store.remotes()[0].link.url().as_str()
        );
        assert_eq!(reloaded.remotes()[1].name.as_str(), "b.lan:9443");
        assert_eq!(reloaded.last(), &Active::Remote(a));
        assert_eq!(reloaded.next_id, store.next_id);
        assert!(reloaded.local_consent());
    }

    #[test]
    fn save_leaves_no_temp_file() {
        let mut store = Profiles::default();
        store
            .add(srv(&format!("https://a.lan:7823/?token={TOKEN_A}")), None)
            .unwrap();

        let dir = TmpDir::new();
        let p = dir.path().join("profiles.json");
        store.save(&p).unwrap();

        let entries: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec!["profiles.json".to_string()],
            "tmp+rename must leave exactly the target file, no temp litter"
        );
    }

    #[test]
    fn link_errors_display_their_locked_string_verbatim() {
        // The page shows `ProfileError` text; a bad pasted link must surface
        // the locked `LinkError` wording unchanged.
        for e in [
            LinkError::NotAUrl,
            LinkError::NotHttps,
            LinkError::NoHost,
            LinkError::NoToken,
            LinkError::HasCredentials,
        ] {
            assert_eq!(ProfileError::from(e.clone()).to_string(), e.to_string());
        }
    }
}
