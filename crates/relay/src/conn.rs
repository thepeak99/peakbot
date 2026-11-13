//! One task per WebSocket (design §4.2): Challenge → Hello (Active/Pending)
//! → Pipe/Post/Ack/ChainFetch/append. The role always comes from the Hub;
//! every store decision runs under the store lock via `Db::run`.
//!
//! Backpressure: outbound goes through a bounded outbox; ANY `try_send` that
//! hits `Full` closes THAT socket with 1013 (design §6.2, one rule).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use cloud::codec::{FromRelay, ToRelay, decode_to_relay, encode_from_relay};
use cloud::constants::relay_error;
use cloud::{DeviceId, LetterId};
use tokio::sync::{mpsc, watch};
use tracing::debug;

use crate::hub::{Conn, Hub, Sent, SocketPermit};
use crate::limits::{OUTBOX_CAP, POSTS_PER_MINUTE};
use crate::store::{Db, HelloOk, HelloReq, Reject, Role};

/// Monotonic socket id: a replaced socket never evicts its successor (§6.2).
static CONN_ID: AtomicU64 = AtomicU64::new(1);

/// Run the per-socket state machine until the socket closes. The permit lives
/// as long as the socket, so a closed socket frees its slot (RAII, §6.5).
pub async fn run_conn(
    mut socket: WebSocket,
    permit: SocketPermit,
    account: i64,
    session: [u8; 32],
    db: Db,
    hub: Hub,
    origin: String,
    shutdown: watch::Receiver<bool>,
) {
    let Ok(nonce) = crate::random_32() else {
        return;
    };
    let (out_tx, mut out_rx) = mpsc::channel::<FromRelay>(OUTBOX_CAP);
    let (close_tx, mut close_rx) = watch::channel(None::<u16>);
    if socket
        .send(Message::Binary(
            encode_from_relay(&FromRelay::Challenge { nonce }).into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    let mut ctx = Ctx {
        out: out_tx,
        close: close_tx,
        hub,
        db,
        account,
        session,
        origin,
        nonce,
        conn_id: CONN_ID.fetch_add(1, Ordering::Relaxed),
        id: None,
        pending_card: None,
        posts: PostWin::new(),
    };
    let hello_window = tokio::time::sleep(Duration::from_secs(cloud::constants::HELLO_WINDOW));
    tokio::pin!(hello_window);
    // The close code, if the loop ended on a close we must write.
    let mut close_code: Option<u16> = None;
    loop {
        tokio::select! {
            _ = &mut hello_window, if ctx.id.is_none() => {
                close_code = Some(1008);
                break;
            }
            _ = crate::stopped(shutdown.clone()) => {
                close_code = Some(1001);
                break;
            }
            _ = close_rx.changed() => {
                close_code = *close_rx.borrow_and_update();
                break;
            }
            out = out_rx.recv() => {
                // Outbox first: a queued reply must reach the peer in order.
                let Some(msg) = out else { break };
                let frame = Message::Binary(encode_from_relay(&msg).into());
                // A stalled peer must not wedge the close path: close and
                // shutdown are re-checked while the frame is in flight.
                let mut send = std::pin::pin!(socket.send(frame));
                tokio::select! {
                    r = &mut send => {
                        if r.is_err() { return; }
                    }
                    _ = close_rx.changed() => {
                        close_code = *close_rx.borrow_and_update();
                        break;
                    }
                    _ = crate::stopped(shutdown.clone()) => {
                        close_code = Some(1001);
                        break;
                    }
                }
            }
            msg = socket.recv() => {
                let Some(msg) = msg else { break };
                let Ok(msg) = msg else {
                    // A read error (incl. an over-cap frame) ends the socket 1009.
                    close_code = Some(1009);
                    break;
                };
                match msg {
                    Message::Binary(b) => {
                        let Ok(req) = decode_to_relay(&b) else {
                            // Unknown version or malformed: 1002 after the error (§4.2).
                            let _ = ctx.emit(FromRelay::Error { code: relay_error::UNSUPPORTED_VERSION });
                            close_code = Some(1002);
                            break;
                        };
                        if !handle(&mut ctx, req).await {
                            // The ctx set the close code via its watch sender.
                            close_code = *close_rx.borrow_and_update();
                            break;
                        }
                    }
                    Message::Text(_) => {
                        let _ = ctx.emit(FromRelay::Error { code: relay_error::UNSUPPORTED_VERSION });
                        close_code = Some(1002);
                        break;
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                    Message::Close(_) => break,
                }
            }
        }
    }
    if let Some(id) = ctx.id {
        ctx.hub.unregister(id, ctx.conn_id);
    }
    if let Some(code) = close_code {
        // Flush queued replies, then write the close frame (1 s bound, §6.2).
        let drain = async {
            while let Ok(m) = out_rx.try_recv() {
                if socket
                    .send(Message::Binary(encode_from_relay(&m).into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            socket
                .send(Message::Close(Some(CloseFrame {
                    code,
                    reason: "".into(),
                })))
                .await
        };
        let _ = tokio::time::timeout(Duration::from_secs(1), drain).await;
    }
    drop(permit);
}

/// Posts per socket, fixed 60 s window (design §5.6).
struct PostWin {
    at: tokio::time::Instant,
    n: u64,
}

impl PostWin {
    fn new() -> Self {
        Self {
            at: tokio::time::Instant::now(),
            n: 0,
        }
    }

    fn allow(&mut self) -> bool {
        let now = tokio::time::Instant::now();
        if now.duration_since(self.at) >= Duration::from_secs(60) {
            self.at = now;
            self.n = 0;
        }
        if self.n >= POSTS_PER_MINUTE {
            return false;
        }
        self.n += 1;
        true
    }
}

/// The per-socket state the reader needs (design §4.2).
struct Ctx {
    out: mpsc::Sender<FromRelay>,
    close: watch::Sender<Option<u16>>,
    hub: Hub,
    db: Db,
    account: i64,
    session: [u8; 32],
    origin: String,
    nonce: [u8; 32],
    conn_id: u64,
    id: Option<DeviceId>,
    pending_card: Option<Vec<u8>>,
    posts: PostWin,
}

impl Ctx {
    /// Queue an outbound message; `Full` ⇒ close 1013 (design §6.2).
    fn emit(&self, msg: FromRelay) -> bool {
        match self.out.try_send(msg) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.close.send_replace(Some(1013));
                if let Some(id) = self.id {
                    self.hub.unregister(id, self.conn_id);
                }
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// `Error{NOT_PERMITTED}`, socket stays open (design §4.2).
    fn deny(&self) -> bool {
        self.emit(FromRelay::Error {
            code: relay_error::NOT_PERMITTED,
        })
    }

    /// Reply an error and close 1008 (Hello failures only, design §4.2).
    fn fail(&self, code: u16) -> bool {
        let _ = self.emit(FromRelay::Error { code });
        self.close.send_replace(Some(1008));
        false
    }

    /// Reply a store rejection; the socket stays open (design §4.4, §4.6).
    fn reply_err(&self, r: &Reject) -> bool {
        self.emit(FromRelay::Error { code: code_for(r) })
    }

    /// Internal failure: close 1011, no repair, no secret-bearing reply (§6.3).
    fn internal(&self) -> bool {
        self.close.send_replace(Some(1011));
        false
    }

    /// `ChainFetch` in every state; a short batch means "caught up" (§4.2).
    async fn chain_fetch(&self, from_seq: u64) -> bool {
        let db = self.db.clone();
        let acct = self.account;
        let entries = match db.run(move |s| s.fetch(acct, from_seq)).await {
            Ok(Ok(e)) => e,
            _ => return self.internal(),
        };
        self.emit(FromRelay::Chain { entries })
    }

    /// Both Hellos (design §4.3): verify under the store lock, register in
    /// the Hub (newest wins), Welcome, then replay stored letters.
    async fn hello(&mut self, req: HelloReq) -> bool {
        let (db, acct, session) = (self.db.clone(), self.account, self.session);
        let (origin, nonce) = (self.origin.clone(), self.nonce);
        let res = db
            .run(move |s| s.hello(acct, session, req, &origin, nonce, crate::now_secs()))
            .await;
        match res {
            Ok(Ok(h)) => self.register(h).await,
            Ok(Err(Reject::Internal)) | Err(_) => self.internal(),
            Ok(Err(r)) => self.fail(code_for(&r)),
        }
    }

    async fn register(&mut self, h: HelloOk) -> bool {
        let id = h.id;
        self.id = Some(id);
        if h.role == Role::Pending {
            self.pending_card = h.card.clone();
        }
        self.hub.register(
            id,
            Conn {
                id: self.conn_id,
                account: self.account,
                session: self.session,
                role: h.role,
                tx: self.out.clone(),
                close: self.close.clone(),
            },
        );
        if !self.emit(FromRelay::Welcome {
            head_seq: h.head_seq,
            pending: h.role == Role::Pending,
        }) {
            return false;
        }
        let db = self.db.clone();
        let letters = match db.run(move |s| s.letters_for(id, crate::now_secs())).await {
            Ok(Ok(l)) => l,
            _ => return self.internal(),
        };
        for (from, letter_id, sealed) in letters {
            if !self.emit(FromRelay::Letter {
                from,
                letter_id,
                sealed,
            }) {
                return false;
            }
        }
        true
    }

    /// Pipe: Active↔Active only, same account (design §4.5).
    async fn pipe(&self, to: DeviceId, bytes: Vec<u8>) -> bool {
        let Some(me) = self.id else {
            return self.deny();
        };
        if me == to {
            return self.deny();
        }
        let db = self.db.clone();
        let acct = self.account;
        let in_chain = match db.run(move |s| s.is_active(acct, to)).await {
            Ok(Ok(b)) => b,
            _ => return self.internal(),
        };
        if !in_chain {
            return self.deny();
        }
        if self.hub.conn_role(to, acct) != Some(Role::Active) {
            return self.emit(FromRelay::Unreachable { to });
        }
        match self.hub.send(to, FromRelay::Pipe { from: me, bytes }) {
            Sent::Ok => true,
            Sent::Gone => self.emit(FromRelay::Unreachable { to }),
        }
    }

    /// Post: rate + size first, then the store caps (design §4.6).
    async fn post(
        &mut self,
        role: Role,
        to: DeviceId,
        letter_id: LetterId,
        sealed: Vec<u8>,
    ) -> bool {
        let Some(me) = self.id else {
            return self.deny();
        };
        if !self.posts.allow() {
            return self.emit(FromRelay::Error {
                code: relay_error::RATE_LIMITED,
            });
        }
        if sealed.len() > cloud::constants::MAX_LETTER {
            return self.emit(FromRelay::Error {
                code: relay_error::TOO_LARGE,
            });
        }
        let (db, acct) = (self.db.clone(), self.account);
        // The store takes its own copy; `sealed` is kept for the live push.
        let stored = sealed.clone();
        let res = db
            .run(move |s| s.post(acct, (me, role), to, letter_id, &stored, crate::now_secs()))
            .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(Reject::Internal)) | Err(_) => return self.internal(),
            Ok(Err(r)) => return self.reply_err(&r),
        }
        // Live push; the letter stays stored until Ack or expiry (§4.6).
        if self.hub.conn_role(to, acct).is_some() {
            let _ = self.hub.send(
                to,
                FromRelay::Letter {
                    from: me,
                    letter_id,
                    sealed,
                },
            );
        }
        true
    }

    /// Ack: recipient-only, idempotent, silent (design §4.6). A DB fault
    /// deletes nothing, so the letter is redelivered on the next Welcome.
    async fn ack(&self, letter_id: LetterId) -> bool {
        let Some(me) = self.id else {
            return self.deny();
        };
        let db = self.db.clone();
        let _ = db.run(move |s| s.ack(me, letter_id)).await;
        true
    }

    /// `ChainAppend` (Active) / `EnrollGenesis` (Pending, card-pinned).
    async fn append(&self, entry: Vec<u8>, genesis_card: Option<&Vec<u8>>) -> bool {
        let db = self.db.clone();
        let acct = self.account;
        let bytes = entry.len();
        let gc = genesis_card.cloned();
        let res = db
            .run(move |s| s.append(acct, &entry, gc.as_deref(), crate::now_secs()))
            .await;
        match res {
            Ok(Ok(appended)) => {
                let head_seq = appended.head_seq;
                // Hub effects outside the store lock (design §4.4 step 5).
                self.hub.for_account(acct, |id, conn| {
                    if Some(id) == appended.revoked {
                        conn.close.send_replace(Some(1008));
                        return;
                    }
                    if Some(id) == appended.added && conn.role == Role::Pending {
                        conn.role = Role::Active;
                    }
                });
                let Some(me) = self.id else {
                    return true;
                };
                debug!(account = acct, bytes, "chain append accepted");
                self.hub
                    .broadcast(acct, me, FromRelay::ChainChanged { head_seq })
            }
            Ok(Err(Reject::Internal)) | Err(_) => self.internal(),
            Ok(Err(r)) => self.reply_err(&r),
        }
    }
}

/// `Reject` → relay error code (design §5.5). No code exists for `Internal`:
/// those sockets close 1011 without a reply.
fn code_for(r: &Reject) -> u16 {
    match r {
        Reject::NotPermitted => relay_error::NOT_PERMITTED,
        Reject::Conflict => relay_error::CONFLICT,
        Reject::InvalidEntry => relay_error::INVALID_ENTRY,
        Reject::MailboxFull => relay_error::MAILBOX_FULL,
        Reject::TooManyPending => relay_error::TOO_MANY_PENDING,
        Reject::BadHello => relay_error::BAD_HELLO,
        Reject::Unauthorized => relay_error::UNAUTHORIZED,
        Reject::Internal => relay_error::NOT_PERMITTED,
    }
}

/// The state table (design §4.2); `false` closes the socket. The role is read
/// from the Hub on every message: an append can promote the sender mid-session.
async fn handle(c: &mut Ctx, req: ToRelay) -> bool {
    let role = c.id.and_then(|id| c.hub.conn_role(id, c.account));
    match req {
        ToRelay::ChainFetch { from_seq } => c.chain_fetch(from_seq).await,
        ToRelay::Hello { device, sig } if role.is_none() => {
            c.hello(HelloReq::Active { device, sig }).await
        }
        ToRelay::HelloPending { card, sig } if role.is_none() => {
            c.hello(HelloReq::Pending { card, sig }).await
        }
        ToRelay::Pipe { to, bytes } if role == Some(Role::Active) => c.pipe(to, bytes).await,
        ToRelay::Post {
            to,
            letter_id,
            sealed,
        } if let Some(role) = role => c.post(role, to, letter_id, sealed).await,
        ToRelay::Ack { letter_id } if role.is_some() => c.ack(letter_id).await,
        ToRelay::ChainAppend { entry } if role == Some(Role::Active) => c.append(entry, None).await,
        // Errata E1: on an EMPTY account a Pending ChainAppend is CONFLICT (§6.3);
        // otherwise Pending → NOT_PERMITTED (§4.2).
        ToRelay::ChainAppend { .. } if role == Some(Role::Pending) => {
            let (db, acct) = (c.db.clone(), c.account);
            match db.run(move |s| s.chain(acct).map(|ch| ch.is_none())).await {
                Ok(Ok(true)) => c.reply_err(&Reject::Conflict),
                Ok(Ok(false)) => c.deny(),
                _ => c.internal(),
            }
        }
        ToRelay::EnrollGenesis { entry } if role == Some(Role::Pending) => {
            let Some(card) = &c.pending_card else {
                return c.deny();
            };
            c.append(entry, Some(card)).await
        }
        _ => c.deny(),
    }
}
