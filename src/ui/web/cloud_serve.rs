//! Cloud Mode inbound ops (p3-design §E): the seam that lets tunnelled ops
//! reach the SAME in-process routers the HTTP routes use — one place for
//! validation, the 404-no-oracle behaviour and the upload limits.
//!
//! `serve_op` runs each op through `tower::ServiceExt::oneshot` on the
//! existing routers (no socket, no token layer: the tunnel's pairing is the
//! credential). The file is folded into `src/cloud/serve.rs` when the crate
//! lands (PR-B/C).

use std::convert::Infallible;
use std::pin::Pin;

use axum::Router;
use axum::body::Body;
use axum::extract::ws::Message;
use axum::http::{Method, Request, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use cloud::constants::{MAX_CHUNK, reset};
use cloud::frames::{GetTarget, Op, StreamMsg};
use futures::{Sink, SinkExt, Stream, StreamExt};
use tower::ServiceExt;
use uuid::Uuid;

use super::{DeviceId, Principal, WsState, conversations, images, serve_socket, uploads};
use crate::cloud::stream::CloudStream;

/// Cloud Mode inbound ops (p3-design §E.1/§E.2): the `InboundHook` target.
/// Holds no state of its own — it only translates a peer's `Op` onto the
/// same code paths a browser/token client hits (one task per stream).
pub(crate) struct CloudServe;

impl CloudServe {
    /// One task per peer stream (§E.2 table): `Attach` → `serve_socket`
    /// with `Principal::CloudPeer` (relayed forced, Shutdown dropped);
    /// `Profiles` → the LOCAL profile list only (§E.3); `List`/`Kill`/
    /// `Get`/`Upload` → `serve_op`. Upload bytes are counted and the
    /// stream `Reset(TOO_LARGE)`s past `general.uploads.max_file_mb`
    /// (§E.4), before the router.
    pub(crate) fn accept(state: WsState, peer: cloud::DeviceId, op: Op, stream: CloudStream) {
        // One task per stream (§E.2); a panic here cannot take the actor down.
        tokio::spawn(serve_stream(state, peer, op, stream));
    }
}

async fn serve_stream(state: WsState, peer: cloud::DeviceId, op: Op, mut stream: CloudStream) {
    match op {
        Op::Attach => attach(state, peer, stream).await,
        Op::Profiles => {
            let body = profiles_body(&state);
            send_reply(&mut stream, 200, "application/json", Bytes::from(body)).await;
        }
        Op::Upload {
            convo,
            name,
            content_type,
        } => {
            let Some(body) = read_upload(&state, &mut stream).await else {
                return;
            };
            let op = PeerOp::Upload {
                convo: Uuid::from_bytes(convo),
                name,
                content_type,
                body,
            };
            reply_with(&state, op, &mut stream).await;
        }
        Op::List => reply_with(&state, PeerOp::List, &mut stream).await,
        Op::Kill { id } => {
            let id = Uuid::from_bytes(id);
            reply_with(&state, PeerOp::Kill { id }, &mut stream).await;
        }
        Op::Get(GetTarget::Image { id }) => {
            reply_with(&state, PeerOp::GetImage { id }, &mut stream).await;
        }
        Op::Get(GetTarget::Upload { convo, id }) => {
            // A malformed id is the router's plain 404 too (no oracle).
            let op = match Uuid::parse_str(&id) {
                Ok(id) => PeerOp::GetUpload {
                    convo: Uuid::from_bytes(convo),
                    id,
                },
                Err(_) => PeerOp::GetImage { id: String::new() },
            };
            reply_with(&state, op, &mut stream).await;
        }
    }
}

/// `Attach`: the same `serve_socket` a browser hits, with the server-side
/// `CloudPeer` rules (§E.1). `Frame` ↔ `Text`; ping/pong aren't mapped
/// (the session layer keeps liveness); `End`/`Reset` end the stream.
async fn attach(state: WsState, peer: cloud::DeviceId, stream: CloudStream) {
    // needs_setup has no registry, and `serve_socket` needs one.
    if state.registry.is_none() {
        let mut stream = stream;
        let _ = stream.send(StreamMsg::Reset(reset::UNSUPPORTED)).await;
        return;
    }
    let (tx, rx) = stream.split();
    let sink: Pin<Box<dyn Sink<Message, Error = crate::cloud::CloudError> + Send>> =
        Box::pin(tx.with_flat_map(|m: Message| {
            futures::stream::iter(match m {
                Message::Text(t) => Some(Ok(StreamMsg::Frame(t.to_string()))),
                _ => None,
            })
        }));
    let incoming: Pin<Box<dyn Stream<Item = Result<Message, Infallible>> + Send>> = Box::pin(
        rx.take_while(|m| futures::future::ready(matches!(m, StreamMsg::Frame(_))))
            .filter_map(|m| async move {
                match m {
                    StreamMsg::Frame(s) => Some(Ok(Message::Text(s.into()))),
                    _ => None,
                }
            }),
    );
    serve_socket(
        sink,
        incoming,
        state,
        Principal::CloudPeer(DeviceId::from_bytes(peer)),
    )
    .await;
}

/// §E.3: the LOCAL profiles only (`machine` is `None`), as `{name, color}`;
/// a `--profile` pin narrows it to the pinned one. Never remote rows.
fn profiles_body(state: &WsState) -> Vec<u8> {
    let rows: Vec<serde_json::Value> = state
        .registry
        .iter()
        .flat_map(|r| {
            let pin = r.profile_pin().map(str::to_string);
            r.boot_catalog()
                .infos()
                .into_iter()
                .filter(move |p| p.machine.is_none() && pin.as_deref().is_none_or(|n| n == p.name))
        })
        .map(|p| serde_json::json!({ "name": p.name, "color": p.color }))
        .collect();
    // The wire body is the bare array the actor parses (`Vec<ProfileInfo>`).
    serde_json::to_vec(&rows).unwrap_or_default()
}

/// §E.4: buffer the peer's `Chunk`s until `End`, `Reset(TOO_LARGE)` as soon
/// as the total passes `max_file_mb` (before the router; it re-checks).
async fn read_upload(state: &WsState, stream: &mut CloudStream) -> Option<Bytes> {
    let cap = u64::from(state.uploads.limits().max_file_mb) * 1024 * 1024;
    let mut body: Vec<u8> = Vec::new();
    loop {
        match stream.next().await? {
            StreamMsg::Chunk(c) => {
                if (body.len() + c.len()) as u64 > cap {
                    let _ = stream.send(StreamMsg::Reset(reset::TOO_LARGE)).await;
                    return None;
                }
                body.extend_from_slice(&c);
            }
            StreamMsg::End => return Some(Bytes::from(body)),
            StreamMsg::Reset(_) => return None,
            _ => {
                let _ = stream.send(StreamMsg::Reset(reset::PROTOCOL)).await;
                return None;
            }
        }
    }
}

/// Run `op` through the in-process routers and stream the response back:
/// `Head`, `Chunk`s (≤ `MAX_CHUNK`), `End`. A non-2xx is still a reply.
async fn reply_with(state: &WsState, op: PeerOp, stream: &mut CloudStream) {
    let resp = serve_op(state, op).await;
    let status = resp.status().as_u16();
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let length = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let head = StreamMsg::Head {
        status,
        content_type: ct,
        length,
    };
    if stream.send(head).await.is_err() {
        return;
    }
    let mut body = resp.into_body().into_data_stream();
    while let Some(chunk) = body.next().await {
        let Ok(bytes) = chunk else {
            let _ = stream.send(StreamMsg::Reset(reset::INTERNAL)).await;
            return;
        };
        for piece in bytes.chunks(MAX_CHUNK) {
            if stream.send(StreamMsg::Chunk(piece.to_vec())).await.is_err() {
                return;
            }
        }
    }
    let _ = stream.send(StreamMsg::End).await;
}

/// A whole in-memory body as `Head` + `Chunk`s + `End`.
async fn send_reply(stream: &mut CloudStream, status: u16, ct: &str, body: Bytes) {
    let head = StreamMsg::Head {
        status,
        content_type: ct.to_string(),
        length: Some(body.len() as u64),
    };
    if stream.send(head).await.is_err() {
        return;
    }
    for piece in body.chunks(MAX_CHUNK) {
        if stream.send(StreamMsg::Chunk(piece.to_vec())).await.is_err() {
            return;
        }
    }
    let _ = stream.send(StreamMsg::End).await;
}

/// The non-Attach ops (mirrors `cloud::Op` minus `Attach`/`Profiles`, §E.2).
#[derive(Debug)]
pub(super) enum PeerOp {
    List,
    Kill {
        id: Uuid,
    },
    GetImage {
        id: String,
    },
    GetUpload {
        convo: Uuid,
        id: Uuid,
    },
    Upload {
        convo: Uuid,
        name: String,
        content_type: String,
        body: Bytes,
    },
}

/// Answer one request/response op in process (A2): the same router (and so
/// the same validation, size limits and no-existence-oracle 404s) as the
/// HTTP routes, via `tower::ServiceExt::oneshot`.
pub(super) async fn serve_op(state: &WsState, op: PeerOp) -> Response {
    let router = Router::new()
        .route("/images/{id}", get(images::image_handler))
        .merge(uploads::router(state.uploads.clone()))
        .merge(conversations::router(state.clone()));
    let Some(req) = build_request(op) else {
        // Only a malformed constant/encoded part fails — unreachable in
        // practice, but the seam must not panic the socket task.
        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "bad op").into_response();
    };
    match router.oneshot(req).await {
        Ok(resp) => resp,
        Err(never) => match never {},
    }
}

fn build_request(op: PeerOp) -> Option<Request<Body>> {
    let (method, uri, body) = match op {
        PeerOp::List => (Method::GET, "/api/conversations".to_string(), None),
        PeerOp::Kill { id } => (Method::POST, format!("/api/conversations/{id}/kill"), None),
        PeerOp::GetImage { id } => (Method::GET, format!("/images/{}", enc(&id)), None),
        PeerOp::GetUpload { convo, id } => {
            (Method::GET, format!("/api/uploads/{convo}/{id}"), None)
        }
        PeerOp::Upload {
            convo,
            name,
            content_type,
            body,
        } => (
            Method::POST,
            format!("/api/uploads?convo={convo}&name={}", enc(&name)),
            Some((content_type, body)),
        ),
    };
    let builder = Request::builder().method(method).uri(uri);
    let built = match body {
        Some((ct, bytes)) => builder
            .header(header::CONTENT_TYPE, ct)
            .body(Body::from(bytes)),
        None => builder.body(Body::empty()),
    };
    built.ok()
}

/// The op parts are constants or percent-encoded; encode for the query/URI.
fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    //! RED until A1/A2 land. Written against the design's final names
    //! (`serve_socket(…, Principal)`, `serve_op(&WsState, PeerOp)`), adapted
    //! from the `spike/p3-seam` proof — not from any merged implementation.

    use super::super::{Principal, WsState, serve_socket};
    use super::*;
    use crate::attachments::UploadStore;
    use crate::config::resolve::{Config, ParseCtx, Source};
    use crate::config::{CONFIG_ENV_LOCK, UploadsConfig};
    use crate::session::{SessionDeps, test_support};
    use crate::storage::{ConversationStorage, InMemoryStorage};
    use crate::ui::web::registry::SessionRegistry;
    use axum::extract::ws::Message;
    use futures::channel::mpsc;
    use futures::{Sink, SinkExt, StreamExt};
    use serde_json::Value;
    use std::convert::Infallible;
    use std::ffi::OsString;
    use std::pin::Pin;
    use std::sync::{Arc, MutexGuard};
    use std::task::{Context, Poll};
    use std::time::Duration;

    /// Two local profiles on one offline Ollama model (t4 fixture).
    const LOCAL_YAML: &str = r#"
providers:
  - name: ollama
    type: ollama
    models:
      - { name: llama3, alias: local }
general:
  context:
    enabled: false
profiles:
  dev:
    agents:
      team:
        model: local
  alt:
    agents:
      a1:
        model: local
      a2:
        model: local
"#;

    /// `LOCAL_YAML` + a REMOTE profile (the E-CHAIN target).
    const WITH_REMOTE_YAML: &str = r#"
providers:
  - name: ollama
    type: ollama
    models:
      - { name: llama3, alias: local }
general:
  context:
    enabled: false
profiles:
  dev:
    agents:
      team:
        model: local
  alt:
    agents:
      a1:
        model: local
      a2:
        model: local
  home-dev:
    remote:
      url: https://10.2.0.48:7823
      token: pito
      ca: system
      profile: dev
"#;

    /// Points `XDG_CONFIG_HOME` at a tempdir holding `yaml` as the global
    /// config, under the crate-wide env lock; restores on drop (t4 pattern).
    struct XdgHome {
        prev: Option<OsString>,
        _dir: tempfile::TempDir,
        _lock: MutexGuard<'static, ()>,
    }

    impl XdgHome {
        fn new(yaml: &str) -> Self {
            let lock = CONFIG_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let dir = tempfile::tempdir().expect("XDG tempdir");
            let prev = std::env::var_os("XDG_CONFIG_HOME");
            unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()) };
            let cfg_dir = crate::config::get_config_dir().expect("config dir");
            std::fs::create_dir_all(&cfg_dir).expect("config dir");
            std::fs::write(cfg_dir.join("config.yaml"), yaml).expect("write config");
            Self {
                prev,
                _dir: dir,
                _lock: lock,
            }
        }
    }

    impl Drop for XdgHome {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
                None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
            }
        }
    }

    struct WebEnv {
        _home: XdgHome,
        _uploads: Option<tempfile::TempDir>,
        state: WsState,
        registry: SessionRegistry,
    }

    /// One `WsState` over the t4-style fixture config (fresh §5.11 reads see
    /// the same YAML through `XDG_CONFIG_HOME`).
    fn state_for(yaml: &str) -> WebEnv {
        state_for_uploads(yaml, None)
    }

    fn state_for_uploads(yaml: &str, limits: Option<UploadsConfig>) -> WebEnv {
        let home = XdgHome::new(yaml);
        let storage = Arc::new(InMemoryStorage::new());
        let mut deps = test_support::test_deps(Some(storage as Arc<dyn ConversationStorage>));
        deps.config = Arc::new(
            Config::parse(
                Source {
                    path: std::path::Path::new("/global/config.yaml"),
                    text: yaml,
                },
                None,
                &ParseCtx { known_skills: &[] },
            )
            .expect("fixture config must parse"),
        );
        let uploads_dir = limits.and_then(|_| tempfile::tempdir().ok());
        let uploads = match (limits, uploads_dir.as_ref()) {
            (Some(l), Some(dir)) => UploadStore::new(dir.path().to_path_buf(), l),
            _ => deps.uploads.clone(),
        };
        let deps: Arc<SessionDeps> = Arc::new(deps);
        let registry = SessionRegistry::new(deps.clone());
        let state = WsState {
            registry: Some(registry.clone()),
            models: Arc::new(Vec::new()),
            active_alias: "local".into(),
            session_ttl: Duration::from_secs(3600),
            uploads,
            directory: std::sync::Arc::new(crate::ui::web::remote_types::Directory::new()),
        };
        WebEnv {
            _home: home,
            _uploads: uploads_dir,
            state,
            registry,
        }
    }

    /// One mpsc sender as a `Sink<Message>` (the in-memory "socket").
    struct OutSink {
        tx: mpsc::Sender<Message>,
    }

    impl Sink<Message> for OutSink {
        type Error = Infallible;
        fn poll_ready(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            self.tx
                .poll_ready(cx)
                .map_err(|_| unreachable!("outbound channel closed mid-test"))
        }
        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            let _ = self.tx.start_send(item);
            Ok(())
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    fn text(s: &str) -> Message {
        Message::Text(s.into())
    }

    fn attach_frame(profile: &str, relayed: bool) -> Message {
        text(&format!(
            r#"{{"type":"attach","profile":"{profile}","agent":"team","relayed":{relayed}}}"#
        ))
    }

    async fn next_text(rx: &mut mpsc::Receiver<Message>) -> String {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(5), rx.next())
                .await
                .expect("frame within 5 s")
                .expect("outbound stream open");
            if let Message::Text(t) = m {
                return t.to_string();
            }
        }
    }

    async fn next_json(rx: &mut mpsc::Receiver<Message>) -> Value {
        serde_json::from_str(&next_text(rx).await).expect("frame is JSON")
    }

    /// Wait for the first frame of `kind`, skipping state/heartbeat noise.
    async fn wait_for(rx: &mut mpsc::Receiver<Message>, kind: &str) -> Value {
        loop {
            let v = next_json(rx).await;
            if v["type"] == kind {
                return v;
            }
        }
    }

    fn dev() -> crate::ui::web::DeviceId {
        crate::ui::web::DeviceId::from_bytes([7u8; 32])
    }

    /// Spawn `serve_socket` over one in-memory channel pair.
    fn serve(
        state: WsState,
        principal: Principal,
    ) -> (
        mpsc::Sender<Message>,
        mpsc::Receiver<Message>,
        tokio::task::JoinHandle<()>,
    ) {
        let (to_host, host_in) = mpsc::channel::<Message>(8);
        let (host_out, from_host) = mpsc::channel::<Message>(64);
        let task = tokio::spawn(serve_socket(
            OutSink { tx: host_out },
            host_in.map(Ok::<_, Infallible>),
            state,
            principal,
        ));
        (to_host, from_host, task)
    }

    // ── A1: Principal::Token — parity with today's axum path ────────────

    /// Token principal over in-memory channels: attach → `attached` +
    /// `ready{v:1}`, and a normal frame round-trips through the session.
    #[tokio::test]
    async fn token_attach_over_in_memory_channels() {
        let env = state_for(LOCAL_YAML);
        let (mut to_host, mut from_host, task) = serve(env.state, Principal::Token);

        to_host.send(attach_frame("dev", false)).await.unwrap();
        assert_eq!(next_json(&mut from_host).await["type"], "attached");
        let ready = next_json(&mut from_host).await;
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready["v"], 1, "ready carries the protocol version");

        // A normal frame round-trips through the session.
        to_host
            .send(text(r#"{"type":"request_recent_dirs"}"#))
            .await
            .unwrap();
        wait_for(&mut from_host, "recent_dirs").await;

        // Closing the inbound side detaches and ends the serve task.
        drop(to_host);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("serve_socket ends when the peer closes")
            .unwrap();
    }

    // ── A1: Principal::CloudPeer — the server decides ───────────────────

    /// CloudPeer: `shutdown` is answered with the locked error frame and
    /// DROPPED — the session keeps serving (E.1: the only refused inbound
    /// message).
    #[tokio::test]
    async fn cloud_peer_shutdown_refused_and_session_continues() {
        let env = state_for(LOCAL_YAML);
        let (mut to_host, mut from_host, task) = serve(env.state, Principal::CloudPeer(dev()));

        to_host.send(attach_frame("dev", false)).await.unwrap();
        assert_eq!(next_json(&mut from_host).await["type"], "attached");
        assert_eq!(next_json(&mut from_host).await["type"], "ready");

        to_host.send(text(r#"{"type":"shutdown"}"#)).await.unwrap();
        let err = wait_for(&mut from_host, "error").await;
        assert_eq!(
            err["message"], "shutdown is not allowed over Cloud Mode",
            "locked refusal text (p3-design §E.1)"
        );

        // The session keeps serving: a normal frame still round-trips.
        to_host
            .send(text(r#"{"type":"request_recent_dirs"}"#))
            .await
            .unwrap();
        wait_for(&mut from_host, "recent_dirs").await;

        drop(to_host);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("serve_socket ends when the peer closes")
            .unwrap();
    }

    /// CloudPeer attach targeting a REMOTE profile is refused with E-CHAIN
    /// even when the client claims `relayed:false` — `relayed` is FORCED on
    /// for CloudPeer (E.1). Bind failure: error frame, stream ends, no
    /// session is minted.
    #[tokio::test]
    async fn cloud_peer_relayed_false_remote_attach_refused_as_chaining() {
        refused_cloud_chain(false).await
    }

    /// …and so is the same attach with `relayed:true` (the client flag is a
    /// hint; the server-side force is the boundary).
    #[tokio::test]
    async fn cloud_peer_relayed_true_remote_attach_refused_as_chaining() {
        refused_cloud_chain(true).await
    }

    async fn refused_cloud_chain(relayed: bool) {
        let env = state_for(WITH_REMOTE_YAML);
        let (mut to_host, mut from_host, _task) = serve(env.state, Principal::CloudPeer(dev()));

        to_host
            .send(attach_frame("home-dev", relayed))
            .await
            .unwrap();
        let err = wait_for(&mut from_host, "error").await;
        assert_eq!(
            err["message"],
            "profile 'home-dev' on this server is itself a remote profile — \
             chaining remote profiles isn't supported",
            "E-CHAIN text (design §5.9), relayed={relayed}"
        );
        // E-CHAIN is a bind failure: the stream ends…
        assert!(
            from_host.next().await.is_none(),
            "E-CHAIN closes the stream"
        );
        // …and no session was minted (checked before `registry.attach`).
        assert!(
            env.registry.active_ids().is_empty(),
            "E-CHAIN must return before registry.attach — no session may exist"
        );
    }

    // ── A2: serve_op — the non-Attach ops in process ────────────────────

    async fn body(resp: Response) -> Bytes {
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
    }

    /// list → 200 `items[]`; kill → 200; unknown image → 404 (no oracle);
    /// upload then get the upload back with identical bytes.
    #[tokio::test]
    async fn serve_op_list_kill_get_upload_roundtrip() {
        let env = state_for(LOCAL_YAML);
        let st = &env.state;

        let list = serve_op(st, PeerOp::List).await;
        assert_eq!(list.status(), 200);
        let v: Value = serde_json::from_slice(&body(list).await).unwrap();
        assert!(v["items"].is_array(), "list carries items[]");

        let kill = serve_op(st, PeerOp::Kill { id: Uuid::new_v4() }).await;
        assert_eq!(kill.status(), 200, "kill of a local id answers 200");

        let img = serve_op(st, PeerOp::GetImage { id: "nope".into() }).await;
        assert_eq!(img.status(), 404, "unknown image id → 404, no oracle");

        let convo = Uuid::new_v4();
        let up = serve_op(
            st,
            PeerOp::Upload {
                convo,
                name: "a b.txt".into(),
                content_type: "application/octet-stream".into(),
                body: Bytes::from_static(b"hello"),
            },
        )
        .await;
        assert_eq!(up.status(), 200, "upload accepted");
        let v: Value = serde_json::from_slice(&body(up).await).unwrap();
        let id = Uuid::parse_str(v["id"].as_str().expect("upload reply carries id")).unwrap();

        let got = serve_op(st, PeerOp::GetUpload { convo, id }).await;
        assert_eq!(got.status(), 200);
        assert_eq!(&body(got).await[..], b"hello", "bytes come back identical");
    }

    /// An upload over `general.uploads.max_file_mb` gets the router's
    /// existing limit status (413) — same limit as HTTP, enforced in one
    /// place (E.4).
    #[tokio::test]
    async fn serve_op_oversized_upload_hits_the_existing_limit() {
        let env = state_for_uploads(
            LOCAL_YAML,
            Some(UploadsConfig {
                max_file_mb: 1,
                ..Default::default()
            }),
        );
        let up = serve_op(
            &env.state,
            PeerOp::Upload {
                convo: Uuid::new_v4(),
                name: "big.bin".into(),
                content_type: "application/octet-stream".into(),
                body: Bytes::from(vec![0u8; 2 * 1024 * 1024]),
            },
        )
        .await;
        assert_eq!(
            up.status(),
            413,
            "PAYLOAD_TOO_LARGE — the existing `max_file_mb` limit"
        );
    }
}
