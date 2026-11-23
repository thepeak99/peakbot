//! `CloudStream` — the bidirectional per-stream handle the actor hands to
//! callers (p3-design §D.6): `Stream<Item = StreamMsg>` + `Sink<StreamMsg>`.
//! Dropping it makes the actor send `Reset(CANCELLED)` (observed through
//! the outbound channel closing) unless `End` was already exchanged.
//!
//! The caller→actor half is a bounded futures channel, so `Sink` applies
//! real backpressure (awaiting capacity) instead of failing a fast caller.
//! For Attach streams this is the shared transport adapter's local end: the
//! actor fragments/reassembles and applies the credit window, so callers
//! only ever see whole `Frame(String)` messages. Non-Attach bulk streams are
//! credit-paced one layer lower: the stream's forwarder buys each record's
//! bytes from the peer window before the actor queues it.

use std::pin::Pin;
use std::task::Context;
use std::time::Duration;

use cloud::constants;
use cloud::frames::StreamMsg;
use futures::channel::mpsc as fmpsc;
use futures::{Sink, SinkExt, Stream, StreamExt};
use tokio::sync::mpsc;

use super::link::CloudError;

/// One open stream to a peer. The actor owns the other end of both
/// channel pairs (routed through the peer's `Mux`); this type is the thin
/// channel end the caller holds.
impl std::fmt::Debug for CloudStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloudStream")
    }
}

pub struct CloudStream {
    tx: fmpsc::Sender<StreamMsg>,
    rx: mpsc::Receiver<StreamMsg>,
}

impl CloudStream {
    pub fn new(tx: fmpsc::Sender<StreamMsg>, rx: mpsc::Receiver<StreamMsg>) -> Self {
        Self { tx, rx }
    }

    /// Receive one record, giving up after `within`. The approved receive
    /// stall budget is per record — a steadily progressing transfer never
    /// hits it. The error is an `Io` failure: the caller resets that stream.
    pub async fn next_timed(&mut self, within: Duration) -> Result<Option<StreamMsg>, CloudError> {
        match tokio::time::timeout(within, self.next()).await {
            Ok(next) => Ok(next),
            Err(_) => Err(CloudError::Io(format!("no data for {}s", within.as_secs()))),
        }
    }

    /// Send one record, giving up after `within`. This waits for the
    /// caller→actor channel to have capacity (the actor's forwarder queue),
    /// not for peer credit; a stalled consumer is a local failure.
    pub async fn send_timed(&mut self, msg: StreamMsg, within: Duration) -> Result<(), CloudError> {
        match tokio::time::timeout(within, Pin::new(&mut *self).send(msg)).await {
            Ok(sent) => sent,
            Err(_) => Err(CloudError::Io(format!(
                "waited {}s for credit",
                within.as_secs()
            ))),
        }
    }
}

impl Stream for CloudStream {
    type Item = StreamMsg;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

impl Sink<StreamMsg> for CloudStream {
    type Error = CloudError;

    fn poll_ready(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().tx)
            .poll_ready(cx)
            .map_err(|_| CloudError::Reset(constants::reset::INTERNAL))
    }

    fn start_send(self: Pin<&mut Self>, item: StreamMsg) -> Result<(), Self::Error> {
        Pin::new(&mut self.get_mut().tx)
            .start_send(item)
            .map_err(|_| CloudError::Reset(constants::reset::INTERNAL))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().tx)
            .poll_flush(cx)
            .map_err(|_| CloudError::Reset(constants::reset::INTERNAL))
    }

    fn poll_close(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        // Dropping the sender is what signals `Gone`; an explicit close
        // must not also send `End` (that is a wire half-close, not a sink
        // operation).
        std::task::Poll::Ready(Ok(()))
    }
}
