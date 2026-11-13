//! The `CloudLink` actor body (p3-design §D): ONE tokio task owns the
//! relay socket, the verified chain, and the session table. Callers talk
//! to it through `CloudCmd`; state is published on a watch channel.
//!
//! Connect sequence per socket (§D.2): token → `GET /ws` (401 ⇒ delete
//! token ⇒ `LoggedOut{Expired}`) → **pre-Hello `ChainFetch`** → pin /
//! `chain::sync` (Err ⇒ `Trust(..)`, NO Hello) → `Challenge` →
//! `sign_hello` → `Hello`/`HelloPending` → `Welcome` → run loop.
//!
//! The blocking `fetch` callback of `chain::check_head` is bridged by
//! pre-fetching the relay delta first and answering from the buffered
//! batch (erratum E-D1).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cloud::{
    DeviceId, LetterId, SessionId,
    card::DeviceCard,
    chain::{self, VerifiedChain},
    codec::{self, FromRelay, ToRelay},
    constants,
    frames::{
        self, FrameReceiver, FrameSender, Inner, Mux, MuxEvent, Op, RecvOutcome, Role, StreamMsg,
    },
    handshake::{self, Close, Data, Hs1, Hs2, Hs3, Initiator, PipeMsg, SessionKeys},
    letter::{self, LetterBody, LetterIn, RecipientState},
};
use futures::{SinkExt as _, StreamExt as _};
use perma_crypto::v1;
use rand_core::{CryptoRng, RngCore};
use rustls::pki_types::pem::PemObject as _;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message as TMsg;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

use super::config::CloudConfig;
use super::enroll::{self, EnrollAttempt};
use super::link::{
    CloudCmd, CloudError, CloudState, CloudStateKind, DeviceInfo, EnrollRequestInfo, FlowError,
    InboundHook, LogoutOutcome, LogoutReason, TrustError,
};
use super::login;
use super::session::{Handshaking, Open, Phase, Session, StreamSide};
use super::store::{PublicState, Seed, SeedStore, SessionToken};
use super::stream::CloudStream;
use crate::config::remote::{CaFingerprint, Endpoint, Trust};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// CSPRNG over `getrandom` (rand 0.9 is a dev-dependency only). A failing
/// OS RNG is unrecoverable, so it aborts rather than hand out weak bytes.
pub(crate) struct Os;
impl CryptoRng for Os {}
impl RngCore for Os {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        if getrandom::fill(dest).is_err() {
            std::process::abort();
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Outbound stream traffic from the per-stream forwarder tasks, plus the
/// drop signal (`Gone`: the caller dropped its `CloudStream`).
pub enum ActorOut {
    Out {
        peer: DeviceId,
        stream: u32,
        msg: StreamMsg,
    },
    Gone {
        peer: DeviceId,
        stream: u32,
    },
}

/// How a connect + live run ended (drives the top-level phase).
enum RunEnd {
    /// No token: back to `LoggedOut`.
    NoToken,
    /// 401 on `/ws` (or relay `UNAUTHORIZED`): token deleted.
    Unauthorized,
    /// Terminal (refuse all sessions, no reconnect).
    Trust(TrustError),
    /// The relay's view of our mode changed (pending ⇄ active): redial
    /// with a fresh `Hello`/`HelloPending` (G.2 step 4, G.3 step 4). No
    /// backoff — the socket was healthy.
    ReHello,
    /// Socket lost / transient: reconnect with backoff.
    /// `(reason, reset-backoff: the socket reached Welcome and stayed ≥60 s)`.
    Dropped(String, bool),
    Stopped,
    /// `Cmd::Logout` completed: `LoggedOut{SignedOut}`.
    LoggedOut,
}

/// This device's identity, derived ONLY from the seed (§C): the keypair
/// is never persisted on its own.
pub struct Identity {
    pub sk: v1::PrivateKey,
    pub card: DeviceCard,
    pub id: DeviceId,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity").field("id", &self.id).finish()
    }
}

impl Identity {
    pub fn new(seed: &Seed, name: &str, platform: u8) -> Result<Self, String> {
        let kp = v1::KeyPair::from_seed(&v1::SecretSeed::from_bytes(*seed.as_bytes()))
            .map_err(|e| e.to_string())?;
        let card = DeviceCard::new(&kp, name, platform).map_err(|e| e.to_string())?;
        let id = card.device_id();
        Ok(Self {
            sk: kp.private().clone(),
            card,
            id,
        })
    }
}

/// The one actor (p3-design §D). `chain_cache`/`raw`/`pin` span sockets;
/// the sessions live only while the socket is up.
pub struct Actor {
    cfg: CloudConfig,
    store: Arc<dyn SeedStore>,
    public: PublicState,
    inbound: Option<InboundHook>,
    id: Identity,
    /// Pinned genesis hash (TOFU trust anchor, §C).
    pin: Option<[u8; 32]>,
    /// Raw cached entries (persisted after every successful sync).
    raw: Vec<Vec<u8>>,
    /// The verified chain (across sockets); `None` = empty account.
    chain_cache: Option<VerifiedChain>,
    /// The token was deleted by a 401 (next `LoggedOut` is `Expired`).
    token_expired: bool,
    attempt: u32,
    outbox_tx: mpsc::Sender<ActorOut>,
    /// A pending ⇄ active flip was observed (G.2/G.3): the next live-loop
    /// iteration ends the socket so the relay re-Hello's with the new mode.
    re_handshake: bool,
    /// The approver's verified, unexpired `EnrollRequest` letters (G.4).
    requests: BTreeMap<LetterId, EnrollRequestInfo>,
    /// The pending device's in-flight attempt (G.3): `None` = not yet
    /// frozen, or rejected (no retry, G.3 step 4).
    enroll: Option<EnrollAttempt>,
    /// An `EnrollReject` was received: no re-freeze/repost until a fresh
    /// login (G.3 step 4, "no retry route").
    rejected: bool,
    /// The relay replay set for letters addressed to us (spec §7.5).
    seen: letter::ReplaySet,
}

/// The live (socket-up) phase: everything here dies with the socket,
/// which keeps "no sessions unless Active" unrepresentable.
struct Live {
    /// `Welcome{pending}`: not in `chain.active` (letters-only mode).
    pending: bool,
    /// Chain fetch returned 0 entries (genesis allowed, §G.2).
    empty_account: bool,
    /// Relay messages received while a `ChainFetch` round-trip was in
    /// flight; re-dispatched when it finishes.
    pending_msgs: VecDeque<FromRelay>,
    /// The in-flight `Cmd::Genesis` reply (resolved on `ChainChanged` /
    /// relay `Error`).
    pending_genesis: Option<oneshot::Sender<Result<DeviceId, CloudError>>>,
    /// The in-flight `ChainAppend` (approve): the request's letter id +
    /// reply. Resolved on the sync that contains the `Add`.
    pending_append: Option<(LetterId, oneshot::Sender<Result<DeviceId, CloudError>>)>,
    /// The in-flight `ChainAppend` (revoke): the target + reply.
    pending_revoke: Option<(DeviceId, oneshot::Sender<Result<(), CloudError>>)>,
    /// One rebuild-and-resend is spent on the in-flight append (G.4 step 3).
    append_retry: bool,
    /// The delta fetched after `Welcome{head_seq > ours}`, pending apply.
    chain_delta: Option<VerifiedChain>,
    sessions: HashMap<DeviceId, Session>,
}

impl Live {
    /// Socket dropped / Trust: every live stream gets `Reset(INTERNAL)`,
    /// queued opens fail `Offline` (§D.4: no transparent resume).
    fn reset_all(&mut self) {
        for s in self.sessions.values_mut() {
            if let Phase::Open(o) = &mut s.phase {
                for side in o.streams.values() {
                    let _ = side
                        .to_caller
                        .try_send(StreamMsg::Reset(constants::reset::INTERNAL));
                }
            }
            for (_, out) in s.queued.drain(..) {
                let _ = out.send(Err(CloudError::Offline));
            }
            if let Some((_, out)) = s.claim.take() {
                let _ = out.send(Err(CloudError::Offline));
            }
        }
        self.sessions.clear();
    }
}

/// Map a `cloud` crate chain failure onto the actor's terminal states.
fn trust_from_chain_err(e: cloud::Error) -> TrustError {
    match e {
        cloud::Error::GenesisMismatch => TrustError::GenesisMismatch,
        // A diverged root / history: the pin no longer verifies.
        cloud::Error::BadPrev | cloud::Error::Gap | cloud::Error::Trust => TrustError::Fork,
        other => TrustError::ChainInvalid(other.to_string()),
    }
}

/// The informational close reason for a `cloud` crate handshake failure.
fn reason_for_cloud_err(e: cloud::Error) -> u8 {
    use cloud::constants::close_reason as r;
    match e {
        cloud::Error::Trust | cloud::Error::GenesisMismatch => r::TRUST,
        cloud::Error::Sequence => r::SEQUENCE,
        cloud::Error::DecryptionError => r::DECRYPTION_ERROR,
        cloud::Error::Limit => r::LIMIT,
        cloud::Error::Revoked => r::REVOKED,
        _ => r::PROTOCOL,
    }
}

/// Close reason → the `Reset` code a local stream gets for it.
fn reset_for_reason(reason: u8) -> u16 {
    if reason == constants::close_reason::PROTOCOL || reason == constants::close_reason::TRUST {
        constants::reset::PROTOCOL
    } else {
        constants::reset::INTERNAL
    }
}

fn devices_of(chain: &VerifiedChain) -> Vec<DeviceInfo> {
    chain
        .active
        .iter()
        .map(|(id, card)| DeviceInfo {
            id: *id,
            name: card.name.clone(),
            platform: card.platform.to_string(),
            // No presence in the relay contract: unknown until a pipe answers.
            online: None,
        })
        .collect()
}

fn publish(state: &watch::Sender<CloudState>, s: CloudState) {
    let _ = state.send(s);
}

fn publish_live(state: &watch::Sender<CloudState>, actor: &Actor, live: &Live) {
    if live.pending {
        publish(
            state,
            CloudState::Pending {
                empty_account: live.empty_account,
                // `Some` once our EnrollRequest is posted (head frozen, §5.5).
                sas: actor.enroll.as_ref().map(|a| a.sas()),
            },
        );
    } else if let Some(c) = &actor.chain_cache {
        publish(
            state,
            CloudState::Active {
                devices: devices_of(c),
                requests: actor.requests.values().cloned().collect(),
            },
        );
    }
}

/// Run a store op off the actor's socket path (keyring stores can block).
async fn store_op<F, T>(f: F) -> io::Result<T>
where
    F: FnOnce() -> io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|_| Err(io::Error::other("store task dropped")))
}

impl Actor {
    pub fn new(
        cfg: CloudConfig,
        store: Arc<dyn SeedStore>,
        public: PublicState,
        inbound: Option<InboundHook>,
        id: Identity,
    ) -> (Self, mpsc::Receiver<ActorOut>) {
        let (out_tx, out_rx) = mpsc::channel(256);
        (
            Self {
                cfg,
                store,
                public,
                inbound,
                id,
                pin: None,
                raw: Vec::new(),
                chain_cache: None,
                token_expired: false,
                attempt: 0,
                outbox_tx: out_tx,
                re_handshake: false,
                requests: BTreeMap::new(),
                enroll: None,
                rejected: false,
                seen: letter::ReplaySet::new(),
            },
            out_rx,
        )
    }

    fn origin_str(&self) -> String {
        // The relay signs Hello tbs with the origin WITHOUT a trailing slash.
        self.cfg
            .relay
            .origin()
            .as_str()
            .trim_end_matches('/')
            .to_string()
    }
}

/// Dial `/ws` with `Authorization: Bearer`. A 401 is a distinct outcome
/// (token expiry), anything else is transient.
enum DialErr {
    Unauth,
    Io(String),
}

async fn dial(cfg: &CloudConfig, token: &SessionToken) -> Result<Ws, DialErr> {
    let origin = cfg.relay.origin();
    let host = origin
        .host_str()
        .map(str::to_string)
        .ok_or_else(|| DialErr::Io("relay origin has no host".into()))?;
    let port = origin
        .port_or_known_default()
        .ok_or_else(|| DialErr::Io("relay origin has no port".into()))?;
    let (scheme, tls) = match &cfg.relay {
        Endpoint::LoopbackPlain { .. } => ("ws".to_string(), None),
        Endpoint::Tls {
            trust: Trust::System,
            ..
        } => {
            let c = crate::http::system_tls_config().map_err(|e| DialErr::Io(e.to_string()))?;
            ("wss".to_string(), Some(Arc::new(c)))
        }
        Endpoint::Tls {
            trust: Trust::PinnedCa(pin),
            ..
        } => ("wss".to_string(), Some(pinned_ca(origin, pin).await?)),
    };
    let tcp = tokio::net::TcpStream::connect((host.clone(), port))
        .await
        .map_err(|e| DialErr::Io(e.to_string()))?;
    let stream = match tls {
        None => MaybeTlsStream::Plain(tcp),
        Some(c) => {
            let name = rustls::pki_types::ServerName::try_from(host.clone())
                .map_err(|e| DialErr::Io(e.to_string()))?;
            let tls = tokio_rustls::TlsConnector::from(c)
                .connect(name, tcp)
                .await
                .map_err(|e| DialErr::Io(e.to_string()))?;
            MaybeTlsStream::Rustls(tls)
        }
    };
    let url = format!("{scheme}://{host}:{port}/ws");
    let mut request = url
        .into_client_request()
        .map_err(|e| DialErr::Io(e.to_string()))?;
    let header = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token.as_str()))
        .map_err(|e| DialErr::Io(e.to_string()))?;
    request
        .headers_mut()
        .insert(reqwest::header::AUTHORIZATION, header);
    match tokio_tungstenite::client_async(request, stream).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(resp))
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED =>
        {
            Err(DialErr::Unauth)
        }
        Err(e) => Err(DialErr::Io(e.to_string())),
    }
}

/// Accepts any certificate (signatures still checked) — for the ONE CA
/// bootstrap GET, whose response is then authenticated by the pin
/// (same policy as the remote-profile bootstrap).
#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// `{origin}/peakbot-ca.crt` fetched unverified, fingerprint-checked
/// against the pin, then the ONLY trusted root (p3-design §B: "same
/// trust code as remote profiles").
async fn pinned_ca(
    origin: &Url,
    pin: &CaFingerprint,
) -> Result<Arc<rustls::ClientConfig>, DialErr> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let no_verify = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| DialErr::Io(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
        .with_no_client_auth();
    let client = reqwest::Client::builder()
        .tls_backend_preconfigured(no_verify)
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| DialErr::Io(e.to_string()))?;
    let url = origin
        .join("peakbot-ca.crt")
        .map_err(|e| DialErr::Io(e.to_string()))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| DialErr::Io(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(DialErr::Io(format!("CA fetch answered {}", resp.status())));
    }
    let pem = resp.text().await.map_err(|e| DialErr::Io(e.to_string()))?;
    let der = rustls::pki_types::CertificateDer::from_pem_slice(pem.as_bytes())
        .map_err(|_| DialErr::Io("the server CA is not a PEM certificate".into()))?;
    if crate::ui::web::tls::der_fingerprint(&der) != pin.to_string() {
        return Err(DialErr::Io("CA pin mismatch".into()));
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der).map_err(|e| DialErr::Io(e.to_string()))?;
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| DialErr::Io(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(cfg))
}

async fn send_to_relay(ws: &mut Ws, msg: &ToRelay) -> Result<(), ()> {
    ws.send(TMsg::Binary(codec::encode_to_relay(msg).into()))
        .await
        .map(|_| ())
        .map_err(|_| ())
}

/// One relay message; `None` = the socket is gone.
async fn next_relay(ws: &mut Ws) -> Option<FromRelay> {
    match ws_recv(ws).await {
        Ok(TMsg::Binary(bytes)) => codec::decode_from_relay(&bytes).ok(),
        _ => None,
    }
}

/// `ChainFetch` in `MAX_CHAIN_BATCH` pages until a short batch.
/// `queue` (when `Some`) collects interleaved relay messages for
/// re-dispatch instead of dropping the socket.
async fn fetch_all(
    ws: &mut Ws,
    from: u64,
    queue: &mut Option<&mut VecDeque<FromRelay>>,
) -> Option<Vec<Vec<u8>>> {
    let mut all = Vec::new();
    let mut from = from;
    loop {
        if send_to_relay(ws, &ToRelay::ChainFetch { from_seq: from })
            .await
            .is_err()
        {
            return None;
        }
        let batch = loop {
            match next_relay(ws).await {
                Some(FromRelay::Chain { entries }) => break entries,
                Some(m) => {
                    if let Some(q) = queue {
                        q.push_back(m);
                        continue;
                    }
                    return None;
                }
                None => return None,
            }
        };
        let len = batch.len();
        all.extend(batch);
        if len < constants::MAX_CHAIN_BATCH {
            break;
        }
        from = from.saturating_add(len as u64);
    }
    Some(all)
}

/// Pre-Hello chain fetch + verification + Challenge/Hello/Welcome
/// (§D.2 steps 2–4). On any chain `Err`: `Trust(..)` and NO Hello.
enum PreHelloErr {
    Trust(TrustError),
    Dropped(String),
}

async fn pre_hello(actor: &mut Actor, ws: &mut Ws, live: &mut Live) -> Result<(), PreHelloErr> {
    let nonce = match next_relay(ws).await {
        Some(FromRelay::Challenge { nonce }) => nonce,
        Some(_) => return Err(PreHelloErr::Dropped("no Challenge".into())),
        None => return Err(PreHelloErr::Dropped("socket closed".into())),
    };
    let from = actor
        .chain_cache
        .as_ref()
        .map(|c| c.head.seq.saturating_add(1))
        .unwrap_or(0);
    let all = match fetch_all(ws, from, &mut None).await {
        Some(v) => v,
        None => {
            return Err(PreHelloErr::Dropped(
                "socket closed during chain fetch".into(),
            ));
        }
    };
    let chain = match (actor.chain_cache.take(), actor.pin) {
        (None, None) => {
            if all.is_empty() {
                None
            } else {
                match chain::extend(None, &all, None) {
                    // No pin yet: TOFU — pin THIS genesis (the trust anchor).
                    Ok(v) => {
                        actor.pin = Some(v.genesis);
                        if let Err(e) = actor.public.save_genesis(v.genesis) {
                            tracing::warn!(%e, "cannot persist the genesis pin");
                        }
                        Some(v)
                    }
                    Err(e) => return Err(PreHelloErr::Trust(trust_from_chain_err(e))),
                }
            }
        }
        (Some(c), _) => match chain::sync(&c, &all) {
            Ok(v) => {
                actor.raw.extend(all.iter().cloned());
                Some(v)
            }
            Err(e) => return Err(PreHelloErr::Trust(trust_from_chain_err(e))),
        },
        (None, Some(_)) if all.is_empty() => {
            return Err(PreHelloErr::Trust(TrustError::GenesisMismatch));
        }
        (None, Some(p)) => match chain::extend(None, &all, Some(p)) {
            Ok(v) => {
                actor.raw = all.clone();
                Some(v)
            }
            Err(e) => return Err(PreHelloErr::Trust(trust_from_chain_err(e))),
        },
    };
    if let Some(c) = &chain {
        if c.revoked.contains(&actor.id.id) {
            return Err(PreHelloErr::Trust(TrustError::Revoked));
        }
        if let Err(e) = actor.public.save_chain(&actor.raw) {
            tracing::warn!(%e, "cannot persist the chain cache");
        }
    }
    let active = chain
        .as_ref()
        .is_some_and(|c| c.active.contains_key(&actor.id.id));
    let mode: u8 = if active { 0 } else { 1 };
    let tbs = codec::hello_tbs(&actor.origin_str(), nonce, actor.id.id, mode);
    let mut rng = Os;
    let sig = codec::sign_hello(&actor.id.sk, &tbs, &mut rng)
        .map_err(|e| PreHelloErr::Trust(TrustError::ChainInvalid(e.to_string())))?;
    let hello = if active {
        ToRelay::Hello {
            device: actor.id.id,
            sig,
        }
    } else {
        ToRelay::HelloPending {
            card: actor.id.card.encode(),
            sig,
        }
    };
    if send_to_relay(ws, &hello).await.is_err() {
        return Err(PreHelloErr::Dropped("socket closed".into()));
    }
    let (head_seq, pending) = match next_relay(ws).await {
        Some(FromRelay::Welcome { head_seq, pending }) => (head_seq, pending),
        Some(_) => return Err(PreHelloErr::Dropped("no Welcome".into())),
        None => return Err(PreHelloErr::Dropped("socket closed".into())),
    };
    // Welcome may be ahead of us (a delta arrived while we were Hello-ing).
    if let (Some(c), Some(h)) = (&chain, head_seq)
        && h > c.head.seq
    {
        let delta = match fetch_all(ws, c.head.seq.saturating_add(1), &mut None).await {
            Some(v) => v,
            None => return Err(PreHelloErr::Dropped("socket closed".into())),
        };
        let c2 = match chain::sync(c, &delta) {
            Ok(v) => v,
            Err(e) => return Err(PreHelloErr::Trust(trust_from_chain_err(e))),
        };
        actor.raw.extend(delta.iter().cloned());
        if let Err(e) = actor.public.save_chain(&actor.raw) {
            tracing::warn!(%e, "cannot persist the chain cache");
        }
        live.chain_delta = Some(c2);
    }
    actor.chain_cache = chain;
    live.pending = pending;
    live.empty_account = actor.chain_cache.is_none();
    Ok(())
}

impl Actor {
    /// Apply a verified fetch (full from 0, or a delta): update pin/raw/
    /// chain, persist, close sessions of peers that left `chain.active`
    /// (`REVOKED`, §D.3).
    async fn apply_entries(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        entries: Vec<Vec<u8>>,
        full: bool,
    ) -> Result<(), TrustError> {
        let old = self.chain_cache.take();
        let v = match (old, full) {
            (Some(c), true) => chain::extend(None, &entries, Some(c.genesis)),
            (None, true) => chain::extend(None, &entries, self.pin),
            (Some(c), false) => chain::sync(&c, &entries),
            (None, false) => return Err(TrustError::ChainInvalid("no chain to sync".into())),
        }
        .map_err(trust_from_chain_err)?;
        if full {
            self.raw = entries.clone();
        } else {
            self.raw.extend(entries.iter().cloned());
        }
        if self.pin.is_none() {
            self.pin = Some(v.genesis);
            if let Err(e) = self.public.save_genesis(v.genesis) {
                tracing::warn!(%e, "cannot persist the genesis pin");
            }
        }
        if let Err(e) = self.public.save_chain(&self.raw) {
            tracing::warn!(%e, "cannot persist the chain cache");
        }
        self.chain_cache = Some(v.clone());
        let gone: Vec<DeviceId> = live
            .sessions
            .keys()
            .filter(|p| !v.active.contains_key(*p))
            .cloned()
            .collect();
        for p in gone {
            self.close_session(live, ws, p, constants::close_reason::REVOKED)
                .await;
        }
        live.pending = !v.active.contains_key(&self.id.id);
        live.empty_account = false;
        Ok(())
    }

    /// `ChainChanged` ⇒ resync (§D.3). A head at or below ours means a
    /// delta cannot verify the root, so refetch from 0 against the pin
    /// (erratum E-D2).
    async fn resync(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        head_seq: u64,
        state: &watch::Sender<CloudState>,
    ) -> Option<RunEnd> {
        // A head at or below ours cannot extend the cache: refetch from 0.
        let (full, from) = match &self.chain_cache {
            Some(c) if head_seq > c.head.seq => (false, c.head.seq.saturating_add(1)),
            _ => (true, 0),
        };
        let was_pending = live.pending;
        let Some(entries) = fetch_all(ws, from, &mut Some(&mut live.pending_msgs)).await else {
            return Some(RunEnd::Dropped("socket closed during resync".into(), false));
        };
        match self.apply_entries(live, ws, entries, full).await {
            Ok(()) => {
                if self
                    .chain_cache
                    .as_ref()
                    .is_some_and(|c| c.revoked.contains(&self.id.id))
                {
                    live.reset_all();
                    return Some(RunEnd::Trust(TrustError::Revoked));
                }
                // Resolve the in-flight ops BEFORE the bookkeeping clears
                // the approved request (the device id is captured first).
                let append_done = live.pending_append.take();
                let append_device = append_done
                    .as_ref()
                    .and_then(|(lid, _)| self.requests.get(lid).map(|r| r.device));
                let append_done = append_done.map(|(_, out)| out);
                let revoke_done = live.pending_revoke.take();
                self.on_chain_applied(live, ws, was_pending).await;
                if let Some(out) = live.pending_genesis.take() {
                    let _ = out.send(Ok(self.id.id));
                }
                if let Some(out) = append_done {
                    live.append_retry = false;
                    let _ = out.send(match append_device {
                        Some(device) => Ok(device),
                        None => Err(CloudError::Flow(FlowError::UnknownRequest)),
                    });
                }
                if let Some((_, out)) = revoke_done {
                    live.append_retry = false;
                    let _ = out.send(Ok(()));
                }
                publish_live(state, self, live);
                None
            }
            Err(t) => {
                for p in live.sessions.keys().cloned().collect::<Vec<_>>() {
                    self.close_session(live, ws, p, constants::close_reason::TRUST)
                        .await;
                }
                Some(RunEnd::Trust(t))
            }
        }
    }

    /// Bookkeeping after a verified chain replace (pre-Hello / resync /
    /// conflict retry): expire and resolve requests, keep the pending
    /// attempt's frozen head honest (G.3 step 4: head moved ⇒ re-freeze
    /// and repost), and flag the re-Hello when our mode flipped
    /// (G.2 step 4, G.3 step 4).
    async fn on_chain_applied(&mut self, live: &mut Live, ws: &mut Ws, was_pending: bool) {
        let now = now_secs();
        self.seen.prune(now);
        let active_now: BTreeSet<DeviceId> = self
            .chain_cache
            .as_ref()
            .map(|c| c.active.keys().copied().collect())
            .unwrap_or_default();
        // Expired, or already on the chain (approved): gone (G.4 step 5).
        self.requests
            .retain(|_, r| r.expires_at > now && !active_now.contains(&r.device));
        let active = active_now.contains(&self.id.id);
        if was_pending && active {
            // Enrolled (or genisied): the relay must re-Hello us as active.
            self.enroll = None;
            self.re_handshake = true;
            return;
        }
        // Letters-only mode (G.3): (re)freeze at the current head and
        // fan out to every active device.
        if live.pending && self.chain_cache.is_some() && !self.rejected {
            let Some(c) = self.chain_cache.as_ref() else {
                return;
            };
            let stale = self.enroll.as_ref().is_some_and(|a| a.frozen() != c.head);
            if self.enroll.is_none() || stale {
                let att = EnrollAttempt::freeze(c, &self.id.card);
                self.enroll = Some(att.clone());
                self.post_enroll_request(ws, &att).await;
            }
        }
    }

    /// G.3 step 2: one signed + sealed `EnrollRequest` per active device
    /// (any one of them may approve).
    async fn post_enroll_request(&mut self, ws: &mut Ws, att: &EnrollAttempt) {
        let Some(chain) = self.chain_cache.as_ref() else {
            return;
        };
        let card = att.card().to_vec();
        let frozen = att.frozen();
        let now = now_secs();
        let mut rng = Os;
        for (dev, card_c) in chain.active.iter() {
            if *dev == self.id.id {
                continue;
            }
            let Ok(pk) = card_c.public_key() else {
                continue;
            };
            let mut id = [0u8; 16];
            if getrandom::fill(&mut id).is_err() {
                continue;
            }
            let body = LetterBody {
                letter_id: id,
                from: self.id.id,
                to: *dev,
                created_at: now,
                kind: letter::LetterKind::EnrollRequest,
                body: letter::LetterKindBody::EnrollRequest {
                    card: card.clone(),
                    head_n: frozen,
                    has_mac: false,
                    mac: None,
                },
            };
            let Ok(signed) = letter::signed_letter(&body, &self.id.sk, &mut rng) else {
                continue;
            };
            let aad = letter::letter_aad(id, self.id.id, *dev);
            let Ok(sealed) = letter::seal_letter(&pk, &signed, &aad, &mut rng) else {
                tracing::debug!("cannot seal the EnrollRequest");
                continue;
            };
            if send_to_relay(
                ws,
                &ToRelay::Post {
                    to: *dev,
                    letter_id: id,
                    sealed,
                },
            )
            .await
            .is_err()
            {
                return; // the socket is dead; the run loop observes it
            }
        }
    }

    /// One relayed letter (G.3 step 4 / G.4 step 1): `check_letter` in the
    /// crate's exact order; any `Err` is a silent drop (debug log).
    async fn on_letter(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        from: DeviceId,
        letter_id: LetterId,
        sealed: Vec<u8>,
        state: &watch::Sender<CloudState>,
    ) {
        let Some(chain) = self.chain_cache.as_ref().cloned() else {
            return;
        };
        let pending = live.pending;
        let now = now_secs();
        let mut in_ = LetterIn {
            sealed,
            relay_from: from,
            relay_letter_id: letter_id,
            self_id: self.id.id,
            self_sk: &self.id.sk,
            self_state: if pending {
                RecipientState::Pending
            } else {
                RecipientState::Active
            },
            now,
            chain: &chain,
            seen: &mut self.seen,
        };
        let accepted = match letter::check_letter(&mut in_) {
            Ok(a) => a,
            Err(e) => {
                tracing::debug!(%e, "letter dropped");
                return;
            }
        };
        match accepted.dispatch {
            letter::Dispatch::Sas | letter::Dispatch::Qr => {
                // A new device asks to join (G.4 step 2).
                if self.requests.len() >= constants::MAX_PENDING_LETTERS {
                    return;
                }
                let Some(info) = enroll::request_info(&chain, &accepted, now) else {
                    // The letter's head is not in our verified history.
                    tracing::debug!("EnrollRequest head not in history; dropped");
                    return;
                };
                self.requests.insert(info.letter_id, info);
                let _ = send_to_relay(ws, &ToRelay::Ack { letter_id }).await;
                publish_live(state, self, live);
            }
            letter::Dispatch::Reject => {
                // An active device refused us (G.3 step 4): stay Pending,
                // SAS cleared, no retry until a fresh login.
                self.enroll = None;
                self.rejected = true;
                publish_live(state, self, live);
            }
        }
    }

    /// One pipe message from `from` (§D.5).
    async fn on_pipe(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        from: DeviceId,
        bytes: Vec<u8>,
        state: &watch::Sender<CloudState>,
    ) {
        let pm = match PipeMsg::parse(&bytes) {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(peer = ?from, "undecodable pipe message");
                return;
            }
        };
        match pm {
            PipeMsg::Hs1(h1) => self.on_hs1(live, ws, from, &h1).await,
            PipeMsg::Hs2(h2) => self.on_hs2(live, ws, from, &h2, state).await,
            PipeMsg::Hs3(h3) => self.on_hs3(live, ws, from, &h3, state).await,
            PipeMsg::Data(d) => {
                if let Some(reason) = self.on_data(live, ws, from, &d).await {
                    self.close_session(live, ws, from, reason).await;
                }
            }
            PipeMsg::Close(c) => self.on_close(live, ws, c.sid).await,
        }
    }

    /// Inbound `Hs1` (§D.5 + G.7 tie-break).
    async fn on_hs1(&mut self, live: &mut Live, ws: &mut Ws, from: DeviceId, h1: &Hs1) {
        let Some(chain) = self.chain_cache.as_ref() else {
            return;
        };
        if !chain.active.contains_key(&from) {
            self.send_close(ws, from, h1.sid, constants::close_reason::TRUST)
                .await;
            return;
        }
        if self.inbound.is_none() {
            // The Local/phone edition refuses every inbound (design §A).
            self.send_close(ws, from, h1.sid, constants::close_reason::PROTOCOL)
                .await;
            return;
        }
        // Tie-break (G.7): the LOWER DeviceId stays initiator.
        let we_win = self.id.id < from
            && live.sessions.get(&from).is_some_and(|s| {
                !matches!(&s.phase, Phase::Handshaking(Handshaking::Responder(_)))
            });
        if we_win {
            // We win (initiator, or established session as initiator):
            // refuse the duplicate; the peer will respond to OUR Hs1.
            self.send_close(ws, from, h1.sid, constants::close_reason::PROTOCOL)
                .await;
            return;
        }
        // We lose (or no session): become the responder, carrying our
        // queued opens over (they are re-bound once the session opens).
        let queued = live
            .sessions
            .remove(&from)
            .map(|s| s.queued)
            .unwrap_or_default();
        self.start_responder(live, ws, from, h1, queued).await;
    }

    /// Responder path: pre-fetch the peer's head delta (the `check_head`
    /// fetch bridge, erratum E-D1), run `Responder::on_hs1`, send `Hs2`.
    async fn start_responder(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        from: DeviceId,
        h1: &Hs1,
        queued: Vec<(Op, oneshot::Sender<Result<CloudStream, CloudError>>)>,
    ) {
        let fail_all = |queued: Vec<(Op, oneshot::Sender<Result<CloudStream, CloudError>>)>| {
            for (_, out) in queued {
                let _ = out.send(Err(CloudError::Offline));
            }
        };
        let Some(chain) = self.chain_cache.as_ref() else {
            fail_all(queued);
            return;
        };
        let mut fetched: Vec<Vec<u8>> = Vec::new();
        if h1.head.seq > chain.head.seq {
            let Some(delta) = fetch_all(
                ws,
                chain.head.seq.saturating_add(1),
                &mut Some(&mut live.pending_msgs),
            )
            .await
            else {
                fail_all(queued);
                return;
            };
            match chain::sync(chain, &delta) {
                Ok(c2) => {
                    self.raw.extend(delta.iter().cloned());
                    self.chain_cache = Some(c2.clone());
                    let _ = self.public.save_chain(&self.raw);
                    fetched = delta;
                }
                Err(e) => {
                    self.send_close(ws, from, h1.sid, constants::close_reason::TRUST)
                        .await;
                    for (_, out) in queued {
                        let _ = out.send(Err(CloudError::Trust(trust_from_chain_err(e))));
                    }
                    return;
                }
            }
        }
        let Some(chain) = self.chain_cache.as_ref() else {
            fail_all(queued);
            return;
        };
        // The bridge: `check_head`'s fetch callback is a sync `Fn`, so it
        // answers from the pre-fetched batch (handed out once). Scoped so
        // the non-Send `Cell` never lives across an await.
        let outcome = {
            let batch = std::cell::Cell::new(Some(fetched));
            let fetch = |from_seq: u64| -> cloud::Result<Vec<Vec<u8>>> {
                match batch.take() {
                    Some(b) if !b.is_empty() => Ok(b),
                    _ if from_seq > chain.head.seq => Err(cloud::Error::StaleChain),
                    _ => Ok(Vec::new()),
                }
            };
            let mut rng = Os;
            handshake::Responder::on_hs1(chain, self.id.id, from, h1, &fetch, &self.id.sk, &mut rng)
        };
        let (resp, hs2) = match outcome {
            Ok(v) => v,
            Err(e) => {
                self.send_close(ws, from, h1.sid, reason_for_cloud_err(e))
                    .await;
                for (_, out) in queued {
                    let _ = out.send(Err(CloudError::Reset(reset_for_reason(
                        constants::close_reason::PROTOCOL,
                    ))));
                }
                return;
            }
        };
        self.send_pipe(ws, from, &PipeMsg::Hs2(hs2)).await;
        let mut s = Session {
            peer: from,
            phase: Phase::Handshaking(Handshaking::Responder(resp)),
            queued: Vec::new(),
            claim: None,
            established_at: now_secs(),
        };
        s.queued = queued;
        live.sessions.insert(from, s);
    }

    /// Outbound `Hs2` for our pending `Initiator` (§D.5).
    async fn on_hs2(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        from: DeviceId,
        h2: &Hs2,
        state: &watch::Sender<CloudState>,
    ) {
        let sid = {
            let Some(s) = live.sessions.get(&from) else {
                return;
            };
            match &s.phase {
                Phase::Handshaking(Handshaking::Initiator(i)) => i.sid(),
                _ => return,
            }
        };
        if sid != h2.sid {
            return;
        }
        let Some(chain) = self.chain_cache.as_ref() else {
            return;
        };
        if h2.head.seq > chain.head.seq {
            let Some(delta) = fetch_all(
                ws,
                chain.head.seq.saturating_add(1),
                &mut Some(&mut live.pending_msgs),
            )
            .await
            else {
                self.close_session(live, ws, from, constants::close_reason::PROTOCOL)
                    .await;
                return;
            };
            match chain::sync(chain, &delta) {
                Ok(c2) => {
                    self.raw.extend(delta.iter().cloned());
                    self.chain_cache = Some(c2.clone());
                    let _ = self.public.save_chain(&self.raw);
                }
                Err(_) => {
                    self.close_session(live, ws, from, constants::close_reason::TRUST)
                        .await;
                    return;
                }
            }
        }
        // The bridge: our chain is already synced, so `check_head`'s F2
        // branch runs without a fetch; a further move ⇒ StaleChain.
        let fetch =
            |_from_seq: u64| -> cloud::Result<Vec<Vec<u8>>> { Err(cloud::Error::StaleChain) };
        let mut rng = Os;
        let hs3 = {
            let Some(s) = live.sessions.get_mut(&from) else {
                return;
            };
            let Phase::Handshaking(Handshaking::Initiator(init)) = &mut s.phase else {
                return;
            };
            match init.on_hs2(from, h2, &fetch, &mut rng) {
                Ok(h) => h,
                Err(e) => {
                    let reason = reason_for_cloud_err(e);
                    return self.close_session(live, ws, from, reason).await;
                }
            }
        };
        let (keys, established) = {
            let Some(s) = live.sessions.get_mut(&from) else {
                return;
            };
            let Phase::Handshaking(Handshaking::Initiator(init)) = &mut s.phase else {
                return;
            };
            match init.keys() {
                Ok(k) => (k, now_secs()),
                Err(_) => {
                    return self
                        .close_session(live, ws, from, constants::close_reason::PROTOCOL)
                        .await;
                }
            }
        };
        self.send_pipe(ws, from, &PipeMsg::Hs3(hs3)).await;
        let queued = {
            let Some(s) = live.sessions.get_mut(&from) else {
                return;
            };
            let Phase::Handshaking(Handshaking::Initiator(init)) = &s.phase else {
                return;
            };
            let sid2 = init.sid();
            let open = establish_open(sid2, &keys, Role::Initiator);
            let q = std::mem::take(&mut s.queued);
            s.phase = Phase::Open(open);
            s.established_at = established;
            q
        };
        for (op, out) in queued {
            self.flush_open(live, ws, from, op, out).await;
        }
        publish_live(state, self, live);
    }

    /// Inbound `Hs3` completing our `Responder` (§D.5).
    async fn on_hs3(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        from: DeviceId,
        h3: &Hs3,
        state: &watch::Sender<CloudState>,
    ) {
        let sid = {
            let Some(s) = live.sessions.get(&from) else {
                return;
            };
            match &s.phase {
                Phase::Handshaking(Handshaking::Responder(r)) => r.sid(),
                _ => return,
            }
        };
        if sid != h3.sid {
            return;
        }
        let Some(chain) = self.chain_cache.as_ref() else {
            return;
        };
        type Queued = Vec<(Op, oneshot::Sender<Result<CloudStream, CloudError>>)>;
        enum Ready {
            Go(SessionKeys, u64, Queued),
            Fail(cloud::Error),
        }
        let res = {
            let Some(s) = live.sessions.get_mut(&from) else {
                return;
            };
            let Phase::Handshaking(Handshaking::Responder(resp)) = &mut s.phase else {
                return;
            };
            match resp.on_hs3(chain, from, h3) {
                Ok(k) => Ready::Go(k, now_secs(), std::mem::take(&mut s.queued)),
                Err(e) => Ready::Fail(e),
            }
        };
        let (keys, established, queued) = match res {
            Ready::Go(k, t, q) => (k, t, q),
            Ready::Fail(e) => {
                self.close_session(live, ws, from, reason_for_cloud_err(e))
                    .await;
                return;
            }
        };
        let open = establish_open(sid, &keys, Role::Responder);
        let mut s = Session {
            peer: from,
            phase: Phase::Open(open),
            queued: Vec::new(),
            claim: None,
            established_at: established,
        };
        // A responder cannot send `Open` itself (mux S1): the first local
        // open binds to the first stream the initiator opens (G.7).
        let mut queued = queued.into_iter();
        s.claim = queued.next();
        for (_, out) in queued {
            let _ = out.send(Err(CloudError::Reset(constants::reset::PROTOCOL)));
        }
        live.sessions.insert(from, s);
        publish_live(state, self, live);
    }

    /// Inbound `Data` for an `Open` session: open → mux → route.
    /// `Some(reason)` closes the session.
    async fn on_data(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        from: DeviceId,
        d: &Data,
    ) -> Option<u8> {
        let s = live.sessions.get_mut(&from)?;
        let Phase::Open(o) = &mut s.phase else {
            return None;
        };
        match o.rx.open(d) {
            Ok(RecvOutcome::Dropped) => None,
            Ok(RecvOutcome::Inner(inner)) => {
                let claim = if matches!(&inner.msg, StreamMsg::Open(op) if s.claim.as_ref().is_some_and(|(c, _)| c == op))
                {
                    s.claim.take()
                } else {
                    None
                };
                match o.mux.feed(&inner) {
                    Ok(events) => {
                        self.route_inner(ws, from, o, &inner, claim, events).await;
                        None
                    }
                    Err(e) => {
                        if let Some((_, out)) = claim {
                            let _ = out.send(Err(CloudError::Reset(constants::reset::PROTOCOL)));
                        }
                        Some(reason_for_cloud_err(e))
                    }
                }
            }
            Err(e) => Some(reason_for_cloud_err(e)),
        }
    }

    /// Route one mux inner + its events to the per-stream channels.
    async fn route_inner(
        &mut self,
        ws: &mut Ws,
        peer: DeviceId,
        o: &mut Open,
        inner: &Inner,
        claim: Option<(Op, oneshot::Sender<Result<CloudStream, CloudError>>)>,
        events: Vec<MuxEvent>,
    ) {
        if let StreamMsg::Open(op) = &inner.msg {
            // Inbound stream (we are the responder side of the mux).
            self.add_stream(o, peer, inner.stream, op.clone(), claim);
            return;
        }
        if let Some(side) = o.streams.get_mut(&inner.stream) {
            if side.to_caller.try_send(inner.msg.clone()).is_err() {
                // Backpressure (§D.5): reset the stream, never block.
                self.drop_stream(o, inner.stream);
                self.send_reset(ws, o, peer, inner.stream, constants::reset::CANCELLED)
                    .await;
            } else if matches!(inner.msg, StreamMsg::End) {
                side.ended = true;
            }
        }
        for ev in events {
            match ev {
                MuxEvent::Closed(id) => self.drop_stream(o, id),
                MuxEvent::Reset(id, code) => {
                    if let Some(side) = o.streams.get(&id) {
                        let _ = side.to_caller.try_send(StreamMsg::Reset(code));
                    }
                    self.drop_stream(o, id);
                }
                MuxEvent::AnswerReset(id, code) => {
                    self.send_reset(ws, o, peer, id, code).await;
                }
            }
        }
    }

    /// Create the per-stream channels + forwarder, and hand the
    /// `CloudStream` to the claim (tie-break) or the inbound hook.
    fn add_stream(
        &self,
        o: &mut Open,
        peer: DeviceId,
        id: u32,
        op: Op,
        claim: Option<(Op, oneshot::Sender<Result<CloudStream, CloudError>>)>,
    ) {
        let (caller_tx, actor_rx) = mpsc::channel(32);
        let (actor_tx, caller_rx) = mpsc::channel(32);
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let stream = CloudStream::new(caller_tx, caller_rx);
        o.streams.insert(
            id,
            StreamSide {
                to_caller: actor_tx,
                forward_stop: stop_tx,
                ended: false,
            },
        );
        self.spawn_forwarder(actor_rx, stop_rx, peer, id);
        match claim {
            Some((_, out)) => {
                let _ = out.send(Ok(stream));
            }
            None => match &self.inbound {
                Some(hook) => hook(peer, op, stream),
                // No hook ⇒ no session should exist; drop defensively.
                None => self.drop_stream(o, id),
            },
        }
    }

    /// One of the caller's outbound messages (forwarder task) / the
    /// caller dropped its `CloudStream` (`Gone`).
    async fn on_out(&mut self, live: &mut Live, ws: &mut Ws, ev: ActorOut) {
        match ev {
            ActorOut::Out { peer, stream, msg } => {
                let ended = matches!(msg, StreamMsg::End);
                let Some(s) = live.sessions.get_mut(&peer) else {
                    return;
                };
                let Phase::Open(o) = &mut s.phase else {
                    return;
                };
                if !o.streams.contains_key(&stream) {
                    return;
                }
                let inner = Inner {
                    stream,
                    msg: msg.clone(),
                };
                // The shared mux tracks BOTH directions, so our own sends
                // are fed too (an illegal one resets that stream locally).
                let events = match o.mux.feed(&inner) {
                    Ok(ev) => ev,
                    Err(_) => {
                        if let Some(side) = o.streams.get(&stream) {
                            let _ = side
                                .to_caller
                                .try_send(StreamMsg::Reset(constants::reset::PROTOCOL));
                        }
                        self.drop_stream(o, stream);
                        return;
                    }
                };
                match o.tx.seal(&frames::encode_inner(&inner)) {
                    Ok(d) => {
                        if let Some(side) = o.streams.get_mut(&stream).filter(|_| ended) {
                            side.ended = true;
                        }
                        for ev in events {
                            if let MuxEvent::Closed(id) | MuxEvent::Reset(id, _) = ev {
                                self.drop_stream(o, id);
                            }
                        }
                        self.send_data(ws, peer, d).await;
                    }
                    Err(_) => {
                        // The record limit: the session is over (§9.6).
                        self.close_session(live, ws, peer, constants::close_reason::LIMIT)
                            .await;
                    }
                }
            }
            ActorOut::Gone { peer, stream } => {
                let ended = {
                    let Some(s) = live.sessions.get(&peer) else {
                        return;
                    };
                    matches!(&s.phase, Phase::Open(o) if o.streams.get(&stream).is_some_and(|side| side.ended))
                };
                let Some(s) = live.sessions.get_mut(&peer) else {
                    return;
                };
                let Phase::Open(o) = &mut s.phase else {
                    return;
                };
                self.drop_stream(o, stream);
                if !ended {
                    // Dropping a CloudStream sends Reset(CANCELLED) (§D.6)
                    // unless `End` was already exchanged.
                    self.send_reset(ws, o, peer, stream, constants::reset::CANCELLED)
                        .await;
                }
            }
        }
    }

    fn drop_stream(&self, o: &mut Open, id: u32) {
        if let Some(side) = o.streams.remove(&id) {
            let _ = side.forward_stop.send(());
        }
    }

    async fn send_pipe(&self, ws: &mut Ws, to: DeviceId, pm: &PipeMsg) {
        let msg = ToRelay::Pipe {
            to,
            bytes: pm.encode(),
        };
        // A dead socket surfaces as a recv error in the run loop.
        let _ = send_to_relay(ws, &msg).await;
    }

    async fn send_data(&self, ws: &mut Ws, to: DeviceId, d: Data) {
        self.send_pipe(ws, to, &PipeMsg::Data(d)).await;
    }

    async fn send_reset(&self, ws: &mut Ws, o: &mut Open, peer: DeviceId, id: u32, code: u16) {
        let reset = Inner {
            stream: id,
            msg: StreamMsg::Reset(code),
        };
        // Keep the shared mux in step; a stale id just errors harmlessly.
        let _ = o.mux.feed(&reset);
        if let Ok(d) = o.tx.seal(&frames::encode_inner(&reset)) {
            self.send_data(ws, peer, d).await;
        }
    }

    async fn send_close(&self, ws: &mut Ws, to: DeviceId, sid: SessionId, reason: u8) {
        self.send_pipe(ws, to, &PipeMsg::Close(Close { sid, reason }))
            .await;
    }

    /// A `Close` for one of our sessions (§D.5: fail what is queued).
    async fn on_close(&mut self, live: &mut Live, ws: &mut Ws, sid: SessionId) {
        for (peer, s) in &live.sessions {
            let matches = match &s.phase {
                Phase::Handshaking(Handshaking::Initiator(i)) => i.sid() == sid,
                Phase::Handshaking(Handshaking::Responder(r)) => r.sid() == sid,
                Phase::Open(o) => o.sid == sid,
            };
            if matches {
                self.close_session(live, ws, *peer, constants::close_reason::PROTOCOL)
                    .await;
                return;
            }
        }
        // Unknown sid: ignore (the crate drops stale sids silently).
    }

    /// Close one session: `Close` to the peer, streams get a `Reset`,
    /// queued opens fail.
    async fn close_session(&mut self, live: &mut Live, ws: &mut Ws, peer: DeviceId, reason: u8) {
        let Some(s) = live.sessions.remove(&peer) else {
            return;
        };
        let sid = match &s.phase {
            Phase::Handshaking(Handshaking::Initiator(i)) => i.sid(),
            Phase::Handshaking(Handshaking::Responder(r)) => r.sid(),
            Phase::Open(o) => o.sid,
        };
        self.send_close(ws, peer, sid, reason).await;
        let reset_code = reset_for_reason(reason);
        match s.phase {
            Phase::Open(o) => {
                for side in o.streams.into_values() {
                    let _ = side.to_caller.try_send(StreamMsg::Reset(reset_code));
                    let _ = side.forward_stop.send(());
                }
            }
            Phase::Handshaking(_) => {
                for (_, out) in s.queued {
                    let _ = out.send(Err(CloudError::Reset(reset_code)));
                }
            }
        }
        if let Some((_, out)) = s.claim {
            let _ = out.send(Err(CloudError::Reset(reset_code)));
        }
    }

    /// `Unreachable{to}`: the peer has no live socket (§D.5).
    fn on_unreachable(&mut self, live: &mut Live, to: DeviceId) {
        let handshaking = live
            .sessions
            .get(&to)
            .is_some_and(|s| matches!(s.phase, Phase::Handshaking(_)));
        if handshaking {
            if let Some(s) = live.sessions.remove(&to) {
                for (_, out) in s.queued {
                    let _ = out.send(Err(CloudError::Offline));
                }
            }
            return;
        }
        if let Some(s) = live.sessions.get_mut(&to) {
            for (_, out) in s.queued.drain(..) {
                let _ = out.send(Err(CloudError::Offline));
            }
        }
    }

    /// Expired sessions (§D.5: `session_expired` ⇒ close `NORMAL`).
    async fn expire_sessions(&mut self, live: &mut Live, ws: &mut Ws) {
        let now = now_secs();
        let expired: Vec<DeviceId> = live
            .sessions
            .values()
            .filter(|s| frames::session_expired(s.established_at, now))
            .map(|s| s.peer)
            .collect();
        for p in expired {
            self.close_session(live, ws, p, constants::close_reason::NORMAL)
                .await;
        }
    }

    /// `open()` from a caller (§D.5): Active + peer in `chain.active`.
    async fn start_open(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        device: DeviceId,
        op: Op,
        out: oneshot::Sender<Result<CloudStream, CloudError>>,
    ) {
        match live.sessions.get_mut(&device) {
            None => {
                let Some(chain) = self.chain_cache.clone() else {
                    let _ = out.send(Err(CloudError::NotActive(CloudStateKind::Connecting)));
                    return;
                };
                let mut rng = Os;
                match Initiator::start(&chain, self.id.id, device, &self.id.sk, &mut rng) {
                    Ok((init, hs1)) => {
                        let mut s = Session {
                            peer: device,
                            phase: Phase::Handshaking(Handshaking::Initiator(init)),
                            queued: Vec::new(),
                            claim: None,
                            established_at: now_secs(),
                        };
                        s.queued.push((op, out));
                        live.sessions.insert(device, s);
                        self.send_pipe(ws, device, &PipeMsg::Hs1(hs1)).await;
                    }
                    Err(_) => {
                        let _ = out.send(Err(CloudError::Offline));
                    }
                }
            }
            Some(s) => match &mut s.phase {
                Phase::Handshaking(_) => s.queued.push((op, out)),
                Phase::Open(_) => {
                    self.flush_open(live, ws, device, op, out).await;
                }
            },
        }
    }

    /// Open the next stream of an `Open` session and reply with the
    /// caller's `CloudStream`.
    async fn flush_open(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        peer: DeviceId,
        op: Op,
        out: oneshot::Sender<Result<CloudStream, CloudError>>,
    ) {
        let id = {
            let Some(s) = live.sessions.get_mut(&peer) else {
                let _ = out.send(Err(CloudError::Offline));
                return;
            };
            let Phase::Open(o) = &mut s.phase else {
                let _ = out.send(Err(CloudError::Reset(constants::reset::INTERNAL)));
                return;
            };
            // Only the session initiator may send `Open` (mux S1): a responder-side
            // open on a live session is unsupported until the peer re-keys (E-D4).
            if o.role == Role::Responder {
                let _ = out.send(Err(CloudError::Reset(constants::reset::UNSUPPORTED)));
                return;
            }
            let id = o.next_stream;
            o.next_stream = o.next_stream.saturating_add(1);
            id
        };
        let Some(s) = live.sessions.get_mut(&peer) else {
            return;
        };
        let Phase::Open(o) = &mut s.phase else {
            return;
        };
        self.open_stream(ws, peer, o, id, op, out).await;
    }

    async fn open_stream(
        &self,
        ws: &mut Ws,
        peer: DeviceId,
        o: &mut Open,
        id: u32,
        op: Op,
        out: oneshot::Sender<Result<CloudStream, CloudError>>,
    ) {
        let (caller_tx, actor_rx) = mpsc::channel(32);
        let (actor_tx, caller_rx) = mpsc::channel(32);
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let stream = CloudStream::new(caller_tx, caller_rx);
        o.streams.insert(
            id,
            StreamSide {
                to_caller: actor_tx,
                forward_stop: stop_tx,
                ended: false,
            },
        );
        self.spawn_forwarder(actor_rx, stop_rx, peer, id);
        let open = Inner {
            stream: id,
            msg: StreamMsg::Open(op),
        };
        if o.mux.feed(&open).is_err() {
            o.streams.remove(&id);
            let _ = out.send(Err(CloudError::Reset(constants::reset::PROTOCOL)));
            return;
        }
        match o.tx.seal(&frames::encode_inner(&open)) {
            Ok(d) => {
                let _ = out.send(Ok(stream));
                self.send_data(ws, peer, d).await;
            }
            Err(_) => {
                o.streams.remove(&id);
                let _ = out.send(Err(CloudError::Reset(constants::reset::INTERNAL)));
            }
        }
    }

    /// The stateless glue between one caller's outbound channel and the
    /// actor's outbox (the actor stays the sole owner of protocol state).
    fn spawn_forwarder(
        &self,
        mut rx: mpsc::Receiver<StreamMsg>,
        stop: oneshot::Receiver<()>,
        peer: DeviceId,
        stream: u32,
    ) {
        let out = self.outbox_tx.clone();
        tokio::spawn(async move {
            let mut stop = stop;
            loop {
                tokio::select! {
                    _ = &mut stop => return,
                    m = rx.recv() => match m {
                        Some(msg) => {
                            if out.send(ActorOut::Out { peer, stream, msg }).await.is_err() {
                                return;
                            }
                        }
                        // The caller dropped its CloudStream.
                        None => {
                            let _ = out.send(ActorOut::Gone { peer, stream }).await;
                            return;
                        }
                    },
                }
            }
        });
    }

    /// One relay message while the socket is live. `Some(end)` terminates
    /// the run.
    async fn on_relay(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        msg: FromRelay,
        state: &watch::Sender<CloudState>,
    ) -> Option<RunEnd> {
        match msg {
            FromRelay::ChainChanged { head_seq } => self.resync(live, ws, head_seq, state).await,
            FromRelay::Pipe { from, bytes } => {
                self.on_pipe(live, ws, from, bytes, state).await;
                None
            }
            FromRelay::Unreachable { to } => {
                self.on_unreachable(live, to);
                None
            }
            FromRelay::Error { code } => {
                if code == constants::relay_error::UNAUTHORIZED {
                    return Some(RunEnd::Unauthorized);
                }
                let in_flight = live.pending_append.is_some()
                    || live.pending_genesis.is_some()
                    || live.pending_revoke.is_some();
                if code == constants::relay_error::CONFLICT && !in_flight {
                    // No in-flight op: CONFLICT = another socket for this
                    // device → back to `Connecting` with backoff (§D.3).
                    return Some(RunEnd::Dropped(
                        "CONFLICT: another socket for this device".into(),
                        false,
                    ));
                }
                // Fails the in-flight op that caused it (§D.3); a
                // `CONFLICT` on an append gets ONE rebuild-and-resend (G.4).
                let conflict = code == constants::relay_error::CONFLICT;
                if let Some(out) = live.pending_genesis.take() {
                    let _ = out.send(Err(CloudError::Flow(FlowError::ChainBusy)));
                }
                if let Some((letter_id, out)) = live.pending_append.take() {
                    if conflict && !live.append_retry {
                        live.append_retry = true;
                        self.retry_append(live, ws, letter_id, out).await;
                    } else {
                        let _ = out.send(Err(CloudError::Flow(FlowError::ChainBusy)));
                    }
                }
                if let Some((target, out)) = live.pending_revoke.take() {
                    if conflict && !live.append_retry {
                        live.append_retry = true;
                        self.retry_revoke(live, ws, target, out).await;
                    } else {
                        let _ = out.send(Ok(()));
                    }
                }
                tracing::warn!(code, "relay error");
                None
            }
            // Letters are enrollment (G.3/G.4): check, then dispatch.
            FromRelay::Letter {
                from,
                letter_id,
                sealed,
            } => {
                self.on_letter(live, ws, from, letter_id, sealed, state)
                    .await;
                None
            }
            // Out of band while live (handled in the pre-Hello phase).
            FromRelay::Challenge { .. } | FromRelay::Welcome { .. } | FromRelay::Chain { .. } => {
                None
            }
        }
    }

    /// Commands while the socket is live.
    async fn on_cmd(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        cmd: CloudCmd,
        state: &watch::Sender<CloudState>,
    ) -> Option<RunEnd> {
        match cmd {
            CloudCmd::Open { device, op, out } => {
                let known = self
                    .chain_cache
                    .as_ref()
                    .map(|c| c.active.contains_key(&device));
                if live.pending || known.is_none() {
                    let _ = out.send(Err(CloudError::NotActive(CloudStateKind::Pending)));
                } else if known == Some(false) {
                    let _ = out.send(Err(CloudError::UnknownDevice));
                } else {
                    self.start_open(live, ws, device, op, out).await;
                }
                None
            }
            CloudCmd::Genesis { out } => {
                if self.chain_cache.is_some() || !live.empty_account {
                    let _ = out.send(Err(CloudError::Flow(FlowError::GenesisTaken)));
                } else {
                    // G.2 step 2: re-fetch first — the account may no
                    // longer be empty (a concurrent genesis won).
                    let Some(entries) = fetch_all(ws, 0, &mut Some(&mut live.pending_msgs)).await
                    else {
                        let _ = out.send(Err(CloudError::Offline));
                        return None;
                    };
                    if entries.is_empty() {
                        let mut rng = Os;
                        let entry =
                            match enroll::genesis_entry(&self.id.card, &self.id.sk, &mut rng) {
                                Ok(e) => e,
                                Err(e) => {
                                    let _ = out.send(Err(CloudError::Io(e.to_string())));
                                    return None;
                                }
                            };
                        let raw = entry.encode();
                        if send_to_relay(ws, &ToRelay::EnrollGenesis { entry: raw })
                            .await
                            .is_err()
                        {
                            let _ = out.send(Err(CloudError::Io("send failed".into())));
                        } else {
                            live.pending_genesis = Some(out);
                        }
                    } else {
                        // Lost the race: sync their genesis, state
                        // recomputes (this device then follows G.3).
                        if let Err(t) = self.apply_entries(live, ws, entries, true).await {
                            return Some(RunEnd::Trust(t));
                        }
                        publish_live(state, self, live);
                        let _ = out.send(Err(CloudError::Flow(FlowError::GenesisTaken)));
                    }
                }
                None
            }
            CloudCmd::Logout { out } => {
                // G.6: self-revoke (best effort, only when others remain)
                // → POST /logout → WIPE → SignedOut. Steps 2–3 always run.
                let mut revoked = false;
                let self_revocable = self
                    .chain_cache
                    .as_ref()
                    .is_some_and(|c| c.active.contains_key(&self.id.id) && c.active.len() > 1);
                if self_revocable
                    && let Some(chain) = self.chain_cache.as_ref()
                    && let Ok(entry) = {
                        let mut rng = Os;
                        enroll::revoke_entry(
                            chain,
                            self.id.id,
                            self.id.card.clone(),
                            &self.id.sk,
                            &mut rng,
                        )
                    }
                {
                    revoked = send_to_relay(
                        ws,
                        &ToRelay::ChainAppend {
                            entry: entry.encode(),
                        },
                    )
                    .await
                    .is_ok();
                }
                let base = self.cfg.relay.origin().clone();
                let store = self.store.clone();
                // A failed relay /logout means the session (and thus the
                // device's listing) survives: report `revoked: false`.
                match store_op(move || store.load_token()).await {
                    Ok(Some(t)) => {
                        revoked = revoked && login::logout_post(&base, &t).await.is_ok();
                    }
                    _ => revoked = false,
                }
                let store = self.store.clone();
                let public = self.public.clone();
                let _ = store_op(move || super::store::wipe_all(&*store, &public)).await;
                self.pin = None;
                self.raw.clear();
                self.chain_cache = None;
                self.requests.clear();
                self.enroll = None;
                live.reset_all();
                live.pending_append = None;
                live.pending_revoke = None;
                let _ = out.send(Ok(LogoutOutcome { revoked }));
                Some(RunEnd::LoggedOut)
            }
            CloudCmd::TokenStored(_) => {
                // A fresh login restarts enrollment (G.3 "no retry route").
                self.rejected = false;
                None
            }
            CloudCmd::Approve { letter_id, out } => {
                self.do_approve(live, ws, letter_id, out).await;
                None
            }
            CloudCmd::Reject { letter_id, out } => {
                self.do_reject(live, ws, letter_id, out, state).await;
                None
            }
            CloudCmd::Revoke { target, out } => {
                self.do_revoke(live, ws, target, out).await;
                None
            }
            CloudCmd::Profiles { device, out } => {
                self.do_profiles(live, ws, device, out).await;
                None
            }
            CloudCmd::Stop => Some(RunEnd::Stopped),
        }
    }

    /// G.4 step 3: sign the `Add` for the request's card; the reply rides
    /// the `ChainChanged` sync (one rebuild on a relay `CONFLICT`).
    async fn do_approve(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        letter_id: LetterId,
        out: oneshot::Sender<Result<DeviceId, CloudError>>,
    ) {
        let Some(req) = self
            .requests
            .get(&letter_id)
            .filter(|r| r.expires_at > now_secs())
        else {
            let _ = out.send(Err(CloudError::Flow(FlowError::UnknownRequest)));
            return;
        };
        let card = req.card.clone();
        let Some(chain) = self.chain_cache.as_ref().cloned() else {
            let _ = out.send(Err(CloudError::NotActive(CloudStateKind::Pending)));
            return;
        };
        let mut rng = Os;
        let entry =
            match enroll::approve_entry(&chain, &card, self.id.card.clone(), &self.id.sk, &mut rng)
            {
                Ok(e) => e,
                Err(e) => {
                    let _ = out.send(Err(CloudError::Io(e.to_string())));
                    return;
                }
            };
        if send_to_relay(
            ws,
            &ToRelay::ChainAppend {
                entry: entry.encode(),
            },
        )
        .await
        .is_err()
        {
            let _ = out.send(Err(CloudError::Io("send failed".into())));
        } else {
            live.pending_append = Some((letter_id, out));
        }
    }

    /// G.4 step 4: seal an `EnrollReject` to the requester, `Ack` the
    /// request, and drop it from state.
    async fn do_reject(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        letter_id: LetterId,
        out: oneshot::Sender<Result<(), CloudError>>,
        state: &watch::Sender<CloudState>,
    ) {
        let Some(req) = self
            .requests
            .get(&letter_id)
            .filter(|r| r.expires_at > now_secs())
            .cloned()
        else {
            let _ = out.send(Err(CloudError::Flow(FlowError::UnknownRequest)));
            return;
        };
        let to = req.device;
        let mut rng = Os;
        let mut id = [0u8; 16];
        if getrandom::fill(&mut id).is_err() {
            let _ = out.send(Err(CloudError::Io("rng failed".into())));
            return;
        }
        let card = match DeviceCard::parse(&req.card) {
            Ok(c) => c,
            Err(_) => {
                let _ = out.send(Err(CloudError::Io("requester card undecodable".into())));
                return;
            }
        };
        let Ok(pk) = card.public_key() else {
            let _ = out.send(Err(CloudError::Io("requester key undecodable".into())));
            return;
        };
        let body = enroll::reject_letter(id, now_secs(), self.id.id, to);
        let Ok(signed) = letter::signed_letter(&body, &self.id.sk, &mut rng) else {
            let _ = out.send(Err(CloudError::Io("sign failed".into())));
            return;
        };
        let aad = letter::letter_aad(id, self.id.id, to);
        let Ok(sealed) = letter::seal_letter(&pk, &signed, &aad, &mut rng) else {
            let _ = out.send(Err(CloudError::Io("seal failed".into())));
            return;
        };
        if send_to_relay(
            ws,
            &ToRelay::Post {
                to,
                letter_id: id,
                sealed,
            },
        )
        .await
        .is_err()
        {
            let _ = out.send(Err(CloudError::Io("send failed".into())));
            return;
        }
        let _ = send_to_relay(ws, &ToRelay::Ack { letter_id }).await;
        self.requests.remove(&letter_id);
        publish_live(state, self, live);
        let _ = out.send(Ok(()));
    }

    /// G.5: sign the `Revoke` for `target`; self ⇒ `use_logout`, an
    /// unknown id ⇒ `unknown_device`.
    async fn do_revoke(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        target: DeviceId,
        out: oneshot::Sender<Result<(), CloudError>>,
    ) {
        if target == self.id.id {
            let _ = out.send(Err(CloudError::Flow(FlowError::UseLogout)));
            return;
        }
        let Some(chain) = self.chain_cache.as_ref().cloned() else {
            let _ = out.send(Err(CloudError::NotActive(CloudStateKind::Pending)));
            return;
        };
        if !chain.active.contains_key(&target) {
            let _ = out.send(Err(CloudError::UnknownDevice));
            return;
        }
        // (`last_device`, G.5 step 4, is unreachable here: self is Active and
        // `target != self`, so ≥ 2 devices; see erratum E-F4.)
        let mut rng = Os;
        let entry =
            match enroll::revoke_entry(&chain, target, self.id.card.clone(), &self.id.sk, &mut rng)
            {
                Ok(e) => e,
                Err(e) => {
                    let _ = out.send(Err(CloudError::Io(e.to_string())));
                    return;
                }
            };
        if send_to_relay(
            ws,
            &ToRelay::ChainAppend {
                entry: entry.encode(),
            },
        )
        .await
        .is_err()
        {
            let _ = out.send(Err(CloudError::Io("send failed".into())));
        } else {
            live.pending_revoke = Some((target, out));
        }
    }

    /// The `Profiles` op (§E.3): open a stream, read `Head` → `Chunk`s →
    /// `End`, parse `{ "profiles": [{name, color}] }`.
    async fn do_profiles(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        device: DeviceId,
        out: oneshot::Sender<Result<Vec<crate::cloud::session::ProfileInfo>, CloudError>>,
    ) {
        let known = self
            .chain_cache
            .as_ref()
            .is_some_and(|c| c.active.contains_key(&device));
        if !known {
            let err = if live.pending {
                CloudError::NotActive(CloudStateKind::Pending)
            } else {
                CloudError::UnknownDevice
            };
            let _ = out.send(Err(err));
            return;
        }
        let (open_tx, open_rx) = oneshot::channel();
        self.start_open(live, ws, device, Op::Profiles, open_tx)
            .await;
        let mut stream = match open_rx.await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                let _ = out.send(Err(e));
                return;
            }
            Err(_) => {
                let _ = out.send(Err(CloudError::Offline));
                return;
            }
        };
        // The read-reply dance runs OFF the actor loop: while it awaits the
        // peer's `Head`, the loop must stay free to forward the socket
        // frames CloudServe answers with (D: one owner, never blocked).
        tokio::spawn(async move {
            // The peer answers `Head, Chunk*, End` (mux S4); a `Reset` is a
            // refusal (a `Close{TRUST}` maps to the PROTOCOL reset code).
            let mut body = Vec::new();
            let result =
                tokio::time::timeout(Duration::from_secs(constants::HANDSHAKE_TIMEOUT), async {
                    let mut status = 0u16;
                    loop {
                        match stream.next().await {
                            Some(StreamMsg::Head { status: s, .. }) => status = s,
                            Some(StreamMsg::Chunk(c)) => body.extend(c),
                            Some(StreamMsg::End) => break,
                            Some(StreamMsg::Reset(code)) => return Err(CloudError::Reset(code)),
                            Some(_) => {}
                            None => return Err(CloudError::Offline),
                        }
                    }
                    if status != 200 {
                        return Err(CloudError::Io(format!("peer answered {status}")));
                    }
                    Ok(())
                })
                .await;
            match result {
                Ok(Ok(())) => {
                    let profiles: Vec<crate::cloud::session::ProfileInfo> =
                        serde_json::from_slice(&body).unwrap_or_default();
                    let _ = stream.send(StreamMsg::End).await;
                    let _ = out.send(Ok(profiles));
                }
                Ok(Err(e)) => {
                    let _ = stream
                        .send(StreamMsg::Reset(constants::reset::CANCELLED))
                        .await;
                    let _ = out.send(Err(e));
                }
                Err(_) => {
                    let _ = stream
                        .send(StreamMsg::Reset(constants::reset::CANCELLED))
                        .await;
                    let _ = out.send(Err(CloudError::Offline));
                }
            }
        });
    }

    /// G.4 step 3, the one retry: refetch (the `CONFLICT` says the head
    /// moved, not how far), rebuild at the new head, resend.
    async fn retry_append(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        letter_id: LetterId,
        out: oneshot::Sender<Result<DeviceId, CloudError>>,
    ) {
        let Some(card) = self.requests.get(&letter_id).map(|r| r.card.clone()) else {
            let _ = out.send(Err(CloudError::Flow(FlowError::UnknownRequest)));
            return;
        };
        if let Err(e) = self.refetch_full(live, ws).await {
            let _ = out.send(Err(e));
            return;
        }
        self.on_chain_applied(live, ws, live.pending).await;
        let Some(chain) = self.chain_cache.as_ref().cloned() else {
            let _ = out.send(Err(CloudError::Offline));
            return;
        };
        let mut rng = Os;
        let entry =
            match enroll::approve_entry(&chain, &card, self.id.card.clone(), &self.id.sk, &mut rng)
            {
                Ok(e) => e,
                Err(e) => {
                    let _ = out.send(Err(CloudError::Io(e.to_string())));
                    return;
                }
            };
        if send_to_relay(
            ws,
            &ToRelay::ChainAppend {
                entry: entry.encode(),
            },
        )
        .await
        .is_err()
        {
            let _ = out.send(Err(CloudError::Io("send failed".into())));
        } else {
            live.pending_append = Some((letter_id, out));
        }
    }

    /// G.5's conflict retry: refetch, rebuild the `Revoke`, resend.
    async fn retry_revoke(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        target: DeviceId,
        out: oneshot::Sender<Result<(), CloudError>>,
    ) {
        if let Err(e) = self.refetch_full(live, ws).await {
            let _ = out.send(Err(e));
            return;
        }
        self.on_chain_applied(live, ws, live.pending).await;
        let Some(chain) = self.chain_cache.as_ref().cloned() else {
            let _ = out.send(Err(CloudError::Offline));
            return;
        };
        let mut rng = Os;
        let entry =
            match enroll::revoke_entry(&chain, target, self.id.card.clone(), &self.id.sk, &mut rng)
            {
                Ok(e) => e,
                Err(e) => {
                    let _ = out.send(Err(CloudError::Io(e.to_string())));
                    return;
                }
            };
        if send_to_relay(
            ws,
            &ToRelay::ChainAppend {
                entry: entry.encode(),
            },
        )
        .await
        .is_err()
        {
            let _ = out.send(Err(CloudError::Io("send failed".into())));
        } else {
            live.pending_revoke = Some((target, out));
        }
    }

    /// Refetch the full chain and re-verify against the pin.
    async fn refetch_full(&mut self, live: &mut Live, ws: &mut Ws) -> Result<(), CloudError> {
        let Some(entries) = fetch_all(ws, 0, &mut Some(&mut live.pending_msgs)).await else {
            return Err(CloudError::Offline);
        };
        self.apply_entries(live, ws, entries, true)
            .await
            .map_err(CloudError::Trust)
    }

    /// Relay messages queued during a `ChainFetch` round-trip.
    async fn drain_pending(
        &mut self,
        live: &mut Live,
        ws: &mut Ws,
        state: &watch::Sender<CloudState>,
    ) {
        while let Some(m) = live.pending_msgs.pop_front() {
            if self.on_relay(live, ws, m, state).await.is_some() {
                return;
            }
        }
    }

    /// Dial + pre-Hello + live loop for one socket.
    async fn connect_and_run(
        &mut self,
        live: &mut Live,
        cmd_rx: &mut mpsc::Receiver<CloudCmd>,
        out_rx: &mut mpsc::Receiver<ActorOut>,
        state: &watch::Sender<CloudState>,
    ) -> RunEnd {
        let token = match store_op({
            let store = self.store.clone();
            move || store.load_token()
        })
        .await
        {
            Ok(Some(t)) => t,
            Ok(None) => return RunEnd::NoToken,
            Err(e) => return RunEnd::Dropped(format!("store: {e}"), false),
        };
        let mut ws = match dial(&self.cfg, &token).await {
            Ok(ws) => ws,
            Err(DialErr::Unauth) => {
                // 401: delete the token ONLY (seed + pin survive, §C).
                let store = self.store.clone();
                let _ = store_op(move || store.clear_token()).await;
                self.token_expired = true;
                return RunEnd::Unauthorized;
            }
            Err(DialErr::Io(e)) => return RunEnd::Dropped(e, false),
        };
        live.sessions.clear();
        live.pending_msgs.clear();
        live.pending_genesis = None;
        live.pending_append = None;
        live.pending_revoke = None;
        live.append_retry = false;
        live.chain_delta = None;
        self.re_handshake = false;
        let was_pending = live.pending;
        if let Err(e) = pre_hello(self, &mut ws, live).await {
            return match e {
                PreHelloErr::Trust(t) => RunEnd::Trust(t),
                PreHelloErr::Dropped(m) => RunEnd::Dropped(m, false),
            };
        }
        // Apply the Welcome delta (if any) before going live.
        if let Some(c) = live.chain_delta.take() {
            self.chain_cache = Some(c);
        }
        // Start (or resume) the pending attempt and flag the re-Hello.
        self.on_chain_applied(live, &mut ws, was_pending).await;
        publish_live(state, self, live);
        run_live(self, live, &mut ws, cmd_rx, out_rx, state).await
    }
}

/// The live loop: `select!` over socket, commands, the stream outbox and
/// session expiry.
async fn run_live(
    actor: &mut Actor,
    live: &mut Live,
    ws: &mut Ws,
    cmd_rx: &mut mpsc::Receiver<CloudCmd>,
    out_rx: &mut mpsc::Receiver<ActorOut>,
    state: &watch::Sender<CloudState>,
) -> RunEnd {
    let welcome_at = Instant::now();
    loop {
        // Our mode flipped on the last sync: end the socket so the relay
        // re-Hello's us with the new mode (G.2/G.3).
        if actor.re_handshake {
            return RunEnd::ReHello;
        }
        let next_expiry = live
            .sessions
            .values()
            .map(|s| s.established_at.saturating_add(constants::SESSION_LIFETIME))
            .min();
        let mut expiry = Box::pin(async {
            if let Some(t) = next_expiry {
                // Wall-clock expiry → monotonic deadline.
                let wait = t.saturating_sub(now_secs());
                tokio::time::sleep(Duration::from_secs(wait)).await;
            } else {
                std::future::pending::<()>().await;
            }
        });
        tokio::select! {
            msg = ws_recv(ws) => {
                match msg {
                    Ok(TMsg::Binary(bytes)) => {
                        match codec::decode_from_relay(&bytes) {
                            Ok(m) => {
                                if let Some(end) = actor.on_relay(live, ws, m, state).await {
                                    return end;
                                }
                                actor.drain_pending(live, ws, state).await;
                            }
                            Err(_) => {
                                return RunEnd::Trust(TrustError::ChainInvalid(
                                    "undecodable relay envelope".into(),
                                ))
                            }
                        }
                    }
                    Ok(_) | Err(_) => {
                        return RunEnd::Dropped(
                            "socket closed".into(),
                            welcome_at.elapsed() >= Duration::from_secs(60),
                        )
                    }
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                if let Some(end) = actor.on_cmd(live, ws, cmd, state).await {
                    return end;
                }
                actor.drain_pending(live, ws, state).await;
            }
            Some(ev) = out_rx.recv() => {
                actor.on_out(live, ws, ev).await;
                actor.drain_pending(live, ws, state).await;
            }
            _ = &mut expiry => {
                actor.expire_sessions(live, ws).await;
            }
        }
    }
}

fn establish_open(sid: SessionId, keys: &SessionKeys, role: Role) -> Open {
    let (tx, rx) = match role {
        Role::Initiator => (
            FrameSender::new(sid, keys.k_ir),
            FrameReceiver::new(sid, keys.k_ri),
        ),
        Role::Responder => (
            FrameSender::new(sid, keys.k_ri),
            FrameReceiver::new(sid, keys.k_ir),
        ),
    };
    Open {
        sid,
        tx,
        rx,
        // The crate mux rejects an inbound Open on a Responder-role mux (S1), so
        // both sides track every sent+received message with one Initiator mux.
        mux: Mux::new(Role::Initiator),
        role,
        streams: HashMap::new(),
        next_stream: 1,
    }
}

/// Full-jitter exponential backoff: 1 s → 2 → 4 … capped at 60 s (§D.4).
fn backoff_delay(attempt: u32) -> Duration {
    let base = 1u64 << (attempt.saturating_sub(1)).min(6);
    let cap = base.min(60);
    let mut b = [0u8; 8];
    if getrandom::fill(&mut b).is_err() {
        return Duration::from_secs(cap);
    }
    let r = u64::from_le_bytes(b) % cap.max(1);
    Duration::from_secs(r)
}

/// The actor's entry point: load the pin + cached chain, then run the
/// phase loop (Idle ⇄ Connecting, Trust terminal).
pub async fn run(
    mut actor: Actor,
    mut cmd_rx: mpsc::Receiver<CloudCmd>,
    mut out_rx: mpsc::Receiver<ActorOut>,
    state: watch::Sender<CloudState>,
) {
    // The cached chain is re-verified from the pin on load (§C: a corrupt
    // file means a refetch, never trust).
    {
        let public = actor.public.clone();
        let loaded = store_op(move || {
            let pin = public.load_genesis()?;
            let raw = public.load_chain()?;
            let cache = match (pin.as_ref(), raw.is_empty()) {
                (Some(p), false) => chain::extend(None, &raw, Some(*p)).ok(),
                _ => None,
            };
            Ok((pin, cache, raw))
        })
        .await;
        if let Ok((pin, cache, raw)) = loaded {
            actor.pin = pin;
            actor.chain_cache = cache;
            actor.raw = raw;
        }
    }
    let mut live = Live {
        pending: false,
        empty_account: actor.chain_cache.is_none(),
        pending_msgs: VecDeque::new(),
        pending_genesis: None,
        pending_append: None,
        pending_revoke: None,
        append_retry: false,
        chain_delta: None,
        sessions: HashMap::new(),
    };

    #[derive(Clone)]
    enum Stage {
        Idle(LogoutReason),
        Trust(TrustError),
        Connecting,
    }
    let mut stage = Stage::Idle(LogoutReason::Never);
    loop {
        match &stage {
            Stage::Idle(reason) => {
                let reason = *reason;
                publish(&state, CloudState::LoggedOut { reason });
                let Some(cmd) = cmd_rx.recv().await else {
                    return;
                };
                match cmd {
                    CloudCmd::TokenStored(_) => {
                        actor.token_expired = false;
                        // A fresh login restarts enrollment (G.3 step 4).
                        actor.rejected = false;
                        stage = Stage::Connecting;
                    }
                    CloudCmd::Open { out, .. } => {
                        let _ = out.send(Err(CloudError::NotActive(CloudStateKind::LoggedOut)));
                    }
                    CloudCmd::Genesis { out } => {
                        let _ = out.send(Err(CloudError::NotActive(CloudStateKind::LoggedOut)));
                    }
                    CloudCmd::Approve { out, .. } => {
                        let _ = out.send(Err(CloudError::NotActive(CloudStateKind::LoggedOut)));
                    }
                    CloudCmd::Reject { out, .. } => {
                        let _ = out.send(Err(CloudError::NotActive(CloudStateKind::LoggedOut)));
                    }
                    CloudCmd::Revoke { out, .. } => {
                        let _ = out.send(Err(CloudError::NotActive(CloudStateKind::LoggedOut)));
                    }
                    CloudCmd::Profiles { out, .. } => {
                        let _ = out.send(Err(CloudError::NotActive(CloudStateKind::LoggedOut)));
                    }
                    CloudCmd::Logout { out } => {
                        let store = actor.store.clone();
                        let public = actor.public.clone();
                        let _ = store_op(move || super::store::wipe_all(&*store, &public)).await;
                        actor.pin = None;
                        actor.raw.clear();
                        actor.chain_cache = None;
                        let _ = out.send(Ok(LogoutOutcome { revoked: false }));
                    }
                    CloudCmd::Stop => return,
                }
            }
            Stage::Trust(e) => {
                let e = e.clone();
                publish(&state, CloudState::Trust(e.clone()));
                let Some(cmd) = cmd_rx.recv().await else {
                    return;
                };
                match cmd {
                    CloudCmd::Open { out, .. } => {
                        let _ = out.send(Err(CloudError::Trust(e.clone())));
                    }
                    CloudCmd::Genesis { out } => {
                        let _ = out.send(Err(CloudError::Trust(e.clone())));
                    }
                    CloudCmd::Approve { out, .. } => {
                        let _ = out.send(Err(CloudError::Trust(e.clone())));
                    }
                    CloudCmd::Reject { out, .. } => {
                        let _ = out.send(Err(CloudError::Trust(e.clone())));
                    }
                    CloudCmd::Revoke { out, .. } => {
                        let _ = out.send(Err(CloudError::Trust(e.clone())));
                    }
                    CloudCmd::Profiles { out, .. } => {
                        let _ = out.send(Err(CloudError::Trust(e.clone())));
                    }
                    CloudCmd::Logout { out } => {
                        let store = actor.store.clone();
                        let public = actor.public.clone();
                        let _ = store_op(move || super::store::wipe_all(&*store, &public)).await;
                        actor.pin = None;
                        actor.raw.clear();
                        actor.chain_cache = None;
                        let _ = out.send(Ok(LogoutOutcome { revoked: false }));
                        stage = Stage::Idle(LogoutReason::SignedOut);
                    }
                    CloudCmd::Stop => return,
                    _ => {}
                }
            }
            Stage::Connecting => {
                publish(
                    &state,
                    CloudState::Connecting {
                        attempt: actor.attempt,
                        last_error: None,
                    },
                );
                let end = actor
                    .connect_and_run(&mut live, &mut cmd_rx, &mut out_rx, &state)
                    .await;
                match end {
                    RunEnd::NoToken => {
                        stage = Stage::Idle(if actor.token_expired {
                            LogoutReason::Expired
                        } else {
                            LogoutReason::Never
                        })
                    }
                    RunEnd::Unauthorized => stage = Stage::Idle(LogoutReason::Expired),
                    RunEnd::Trust(e) => {
                        live.reset_all();
                        stage = Stage::Trust(e);
                    }
                    RunEnd::ReHello => {
                        // The socket was healthy; redial at once (no backoff).
                        stage = Stage::Connecting;
                    }
                    RunEnd::Stopped => return,
                    RunEnd::LoggedOut => stage = Stage::Idle(LogoutReason::SignedOut),
                    RunEnd::Dropped(err, reset) => {
                        live.reset_all();
                        if let Some(out) = live.pending_genesis.take() {
                            let _ = out.send(Err(CloudError::Offline));
                        }
                        if let Some((_, out)) = live.pending_append.take() {
                            let _ = out.send(Err(CloudError::Offline));
                        }
                        if let Some((_, out)) = live.pending_revoke.take() {
                            let _ = out.send(Err(CloudError::Offline));
                        }
                        live.append_retry = false;
                        if reset {
                            // The socket reached Welcome and stayed ≥60 s.
                            actor.attempt = 0;
                        }
                        actor.attempt = actor.attempt.saturating_add(1);
                        publish(
                            &state,
                            CloudState::Connecting {
                                attempt: actor.attempt,
                                last_error: Some(err),
                            },
                        );
                        tokio::select! {
                            _ = tokio::time::sleep(backoff_delay(actor.attempt)) => {}
                            cmd = cmd_rx.recv() => match cmd {
                                Some(CloudCmd::Stop) => return,
                                Some(CloudCmd::Open { out, .. }) => {
                                    let _ = out.send(Err(CloudError::NotActive(
                                        CloudStateKind::Connecting,
                                    )));
                                }
                                Some(CloudCmd::Genesis { out }) => {
                                    let _ = out.send(Err(CloudError::NotActive(
                                        CloudStateKind::Connecting,
                                    )));
                                }
                                Some(CloudCmd::Approve { out, .. }) => {
                                    let _ = out.send(Err(CloudError::NotActive(
                                        CloudStateKind::Connecting,
                                    )));
                                }
                                Some(CloudCmd::Reject { out, .. }) => {
                                    let _ = out.send(Err(CloudError::NotActive(
                                        CloudStateKind::Connecting,
                                    )));
                                }
                                Some(CloudCmd::Revoke { out, .. }) => {
                                    let _ = out.send(Err(CloudError::NotActive(
                                        CloudStateKind::Connecting,
                                    )));
                                }
                                Some(CloudCmd::Profiles { out, .. }) => {
                                    let _ = out.send(Err(CloudError::NotActive(
                                        CloudStateKind::Connecting,
                                    )));
                                }
                                _ => {}
                            },
                        }
                        stage = Stage::Connecting;
                    }
                }
            }
        }
    }
}

/// Next websocket message; a closed stream is an `Err` like a read error.
async fn ws_recv(ws: &mut Ws) -> Result<TMsg, ()> {
    match ws.next().await {
        Some(Ok(m)) => Ok(m),
        _ => Err(()),
    }
}
