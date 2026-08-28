//! Per-connection outbound plumbing, shared by the WS and stdio transports.
//!
//! Two frame classes leave a connection with two different contracts:
//!
//! * **ORDERED** (everything except `State`): bounded FIFO, delivered in send
//!   order, never silently dropped — an overflow tears the connection down.
//! * **COALESCING** (`State`): one slot. Publishing a newer snapshot *replaces*
//!   an older unwritten one. A client that is behind does not want 2 188
//!   historical snapshots, it wants the newest one.
//!
//! | class      | frames                                                                                  | order                          | loss                                                       | bound                              |
//! |------------|-----------------------------------------------------------------------------------------|--------------------------------|------------------------------------------------------------|------------------------------------|
//! | ordered    | `attached`, `ready`, `models_available`, `conversations_list`, `recent_dirs`, `dir_listing`, `error` | FIFO, exactly once             | never dropped — an overflow **kills the connection**       | ≤ `CTRL_CAPACITY` (32) small frames |
//! | coalescing | `state`                                                                                 | published order preserved; gaps allowed | older unwritten snapshots are replaced by newer ones | exactly 1 slot                     |
//!
//! Cross-class: ordered frames win in `next()` (biased), so the handshake
//! trio always reaches the writer before the first snapshot. Snapshot
//! delivery becoming **officially lossy** is accepted because the SPA does
//! `setState(msg.state)` (a full replace, `web/src/useAgent.ts:138-139`) and
//! nothing accumulates or diffs snapshots — but any future feature that
//! treats a snapshot as an *event* would silently break. Re-read this table
//! before changing either side.

use crate::ui::app_state::AppState;
use crate::ui::wire::OutboundMessage;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// Max ordered frames in flight per connection. Every ordered frame is a
/// reply to something the peer sent (or one of the three handshake frames),
/// so a healthy client never approaches this; reaching it means the peer has
/// stopped reading, which we treat as a dead connection.
pub(crate) const CTRL_CAPACITY: usize = 32;

/// The connection is gone. Returned for a dropped writer *and* for an
/// ordered overflow — producers react identically to both: stop and tear
/// down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Disconnected;

/// Producer handle. Cheap to clone (both inner senders are `Clone`); the
/// channel closes when the last clone drops.
#[derive(Clone)]
pub(crate) struct OutboundTx {
    ctrl: mpsc::Sender<OutboundMessage>,
    state: watch::Sender<Option<Arc<AppState>>>,
}

/// Consumer handle. Moved into the writer task; deliberately **not** `Clone` —
/// exactly one task may own the sink.
pub(crate) struct OutboundRx {
    ctrl: mpsc::Receiver<OutboundMessage>,
    state: watch::Receiver<Option<Arc<AppState>>>,
    ctrl_done: bool,
    state_done: bool,
}

/// Build a fresh ordered-FIFO + coalescing-slot pair.
pub(crate) fn outbound_channel() -> (OutboundTx, OutboundRx) {
    let (ctrl_tx, ctrl_rx) = mpsc::channel(CTRL_CAPACITY);
    // watch starts with `None` so the very first `next()` waits until the
    // first publish, rather than yielding a fabricated initial value.
    let (state_tx, state_rx) = watch::channel(None);
    (
        OutboundTx {
            ctrl: ctrl_tx,
            state: state_tx,
        },
        OutboundRx {
            ctrl: ctrl_rx,
            state: state_rx,
            ctrl_done: false,
            state_done: false,
        },
    )
}

impl OutboundTx {
    /// Enqueue an ordered frame. Synchronous (`try_send`) so the inbound
    /// dispatcher stays synchronous. `Err(Disconnected)` means either the
    /// consumer dropped or the bounded FIFO overflowed — both are "tear
    /// down" signals.
    ///
    /// Panics in debug if handed `OutboundMessage::State` — that frame
    /// must go through `publish_state`, see the grep test (`T12`).
    pub(crate) fn send(&self, msg: OutboundMessage) -> Result<(), Disconnected> {
        if let OutboundMessage::State { .. } = &msg {
            debug_assert!(
                false,
                "OutboundMessage::State must go through publish_state, not send"
            );
            // In release builds we still don't want to bypass the contract —
            // route it through publish_state with a brand-new snapshot's
            // payload would be wrong, so refuse the frame.
            return Err(Disconnected);
        }
        self.ctrl.try_send(msg).map_err(|_| Disconnected)
    }

    /// Publish the newest state snapshot, replacing any snapshot not yet
    /// written. Never blocks, never grows. `Err(Disconnected)` means the
    /// consumer dropped — producers stop and tear down.
    pub(crate) fn publish_state(&self, state: Arc<AppState>) -> Result<(), Disconnected> {
        // `send` on a closed watch returns Err; the watch has no other
        // failure mode. The send here also wakes the receiver.
        self.state.send(Some(state)).map_err(|_| Disconnected)
    }
}

impl OutboundRx {
    /// Next frame to write. Ordered frames win over state (biased `select!`),
    /// so the handshake trio always precedes the first snapshot. Returns
    /// `None` only when both halves are closed *and* drained.
    ///
    /// CANCEL-SAFE: no `await` sits between `state.changed()` resolving
    /// (which marks the version seen) and the cloned `Arc` returning, so a
    /// cancelled `next()` cannot swallow a snapshot. The web writer relies
    /// on this to select `next()` against its keepalive timer.
    pub(crate) async fn next(&mut self) -> Option<OutboundMessage> {
        loop {
            tokio::select! {
                biased;
                msg = self.ctrl.recv(), if !self.ctrl_done => match msg {
                    Some(m) => return Some(m),
                    // Closed *and* drained: receiver and all senders gone.
                    None => self.ctrl_done = true,
                },
                res = self.state.changed(), if !self.state_done => match res {
                    Ok(()) => {
                        // borrow_and_update marks the version seen, then we
                        // clone the Arc. No await between them, so the body
                        // is atomic w.r.t. cancellation.
                        if let Some(s) = self.state.borrow_and_update().clone() {
                            return Some(OutboundMessage::State { state: s });
                        }
                        // Slot holds None (the initial value before any
                        // publish). Loop and wait for a real change.
                    }
                    Err(_) => self.state_done = true,
                },
            }
            if self.ctrl_done && self.state_done {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app_state::AppState;
    use crate::ui::wire::OutboundMessage;
    use std::sync::{Arc, Weak};

    /// The incident in miniature. Publishing a newer `Arc<AppState>` must
    /// replace the slot's previous content, so the only live snapshot at any
    /// moment is the one most recently published — never the whole history.
    /// Asserts an exact memory invariant via `Weak` (no allocator hooks).
    #[test]
    fn state_slot_holds_only_the_newest_snapshot() {
        let (tx, _rx) = outbound_channel();
        let mut weaks: Vec<Weak<AppState>> = Vec::with_capacity(1000);

        for i in 0..1000 {
            let mut state = AppState::new();
            state.status_message = Some(format!("snap-{i}"));
            let arc = Arc::new(state);
            weaks.push(Arc::downgrade(&arc));
            tx.publish_state(arc)
                .expect("producer must not be disconnected while tx is alive");
        }

        let live = weaks.iter().filter(|w| w.upgrade().is_some()).count();
        assert_eq!(
            live, 1,
            "the slot must hold only the newest snapshot; found {live} live out of 1000"
        );

        // The survivor must be the last published, not some earlier one.
        let survivor = weaks
            .iter()
            .find_map(|w| w.upgrade())
            .expect("exactly one weak must be upgradable");
        assert_eq!(
            survivor.status_message.as_deref(),
            Some("snap-999"),
            "the surviving snapshot must be the most recently published one"
        );
    }

    /// T2 — `CTRL_CAPACITY` ordered frames fit losslessly and FIFO.
    #[test]
    fn ordered_frames_are_fifo_and_lossless() {
        let (tx, mut rx) = outbound_channel();
        for i in 0..CTRL_CAPACITY {
            tx.send(OutboundMessage::Error {
                message: format!("m{i}"),
            })
            .expect("send must succeed while rx is alive");
        }
        let mut received = Vec::with_capacity(CTRL_CAPACITY);
        for _ in 0..CTRL_CAPACITY {
            let frame = futures::executor::block_on(rx.next());
            received.push(frame);
        }
        assert_eq!(received.len(), CTRL_CAPACITY);
        for (i, frame) in received.iter().enumerate() {
            match frame.as_ref().expect("frame present") {
                OutboundMessage::Error { message } => assert_eq!(message, &format!("m{i}")),
                other => panic!("unexpected variant: {other:?}"),
            }
        }
    }

    /// T3 — overflowing the ordered FIFO surfaces `Disconnected` rather than
    /// silently dropping or reordering middle frames.
    #[test]
    fn ordered_overflow_reports_disconnected_and_never_reorders() {
        let (tx, mut rx) = outbound_channel();
        for i in 0..CTRL_CAPACITY {
            tx.send(OutboundMessage::Error {
                message: format!("m{i}"),
            })
            .unwrap();
        }
        let overflow = tx.send(OutboundMessage::Error {
            message: "overflow".to_string(),
        });
        assert!(
            matches!(overflow, Err(Disconnected)),
            "overflow must report Disconnected, got {overflow:?}"
        );

        let mut received = Vec::with_capacity(CTRL_CAPACITY);
        for _ in 0..CTRL_CAPACITY {
            received.push(futures::executor::block_on(rx.next()));
        }
        assert_eq!(received.len(), CTRL_CAPACITY);
        for (i, frame) in received.iter().enumerate() {
            match frame.as_ref().expect("frame present") {
                OutboundMessage::Error { message } => assert_eq!(message, &format!("m{i}")),
                other => panic!("unexpected variant: {other:?}"),
            }
        }
    }

    /// T4 — biased ordering: an ordered frame beats a state that was already
    /// published. The handshake trio must reach the writer before any state.
    #[test]
    fn ordered_frames_win_over_state() {
        let (tx, mut rx) = outbound_channel();
        tx.publish_state(Arc::new(AppState::new()))
            .expect("publish before any send");
        tx.send(OutboundMessage::Ready)
            .expect("send before any send");

        let first = futures::executor::block_on(rx.next());
        assert!(
            matches!(first, Some(OutboundMessage::Ready)),
            "first frame must be Ready (biased), got {first:?}"
        );
        let second = futures::executor::block_on(rx.next());
        assert!(
            matches!(second, Some(OutboundMessage::State { .. })),
            "second frame must be State, got {second:?}"
        );
    }

    /// T5 — a snapshot published immediately before the producer drops is
    /// still delivered by `next()` before the closed signal — tokio's `watch`
    /// version-before-closed ordering. Locks in `exit_requested` not being
    /// lost on teardown.
    #[test]
    fn final_state_survives_sender_drop() {
        let (tx, mut rx) = outbound_channel();
        tx.publish_state(Arc::new(AppState::new())).unwrap();
        drop(tx);

        let first = futures::executor::block_on(rx.next());
        assert!(
            matches!(first, Some(OutboundMessage::State { .. })),
            "the published snapshot must be delivered before the close signal, got {first:?}"
        );
        let second = futures::executor::block_on(rx.next());
        assert!(
            second.is_none(),
            "after both halves close and drain, next() must return None, got {second:?}"
        );
    }

    /// T6 — when the consumer drops, both producers report `Disconnected`.
    /// This is the signal the web forwarder relies on to break its loop.
    #[test]
    fn producers_report_disconnected_after_receiver_drop() {
        let (tx, rx) = outbound_channel();
        drop(rx);

        let ordered = tx.send(OutboundMessage::Ready);
        assert!(
            matches!(ordered, Err(Disconnected)),
            "send must report Disconnected when rx was dropped, got {ordered:?}"
        );

        let state = tx.publish_state(Arc::new(AppState::new()));
        assert!(
            matches!(state, Err(Disconnected)),
            "publish_state must report Disconnected when rx was dropped, got {state:?}"
        );
    }
}
