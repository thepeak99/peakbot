//! `CloudStream` — the bidirectional per-stream handle the actor hands to
//! callers (p3-design §D.6): `Stream<Item = StreamMsg>` + `Sink<StreamMsg>`.
//! Dropping it makes the actor send `Reset(CANCELLED)` (observed through
//! the outbound channel closing) unless `End` was already exchanged.
//! `AttachStream` (the spike) wraps it as `Frame(String)` ↔ `TMsg::Text`
//! for the controller.

use std::pin::Pin;
use std::task::Context;

use cloud::frames::StreamMsg;
use futures::{Sink, Stream};
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
    tx: mpsc::Sender<StreamMsg>,
    rx: mpsc::Receiver<StreamMsg>,
}

impl CloudStream {
    pub fn new(tx: mpsc::Sender<StreamMsg>, rx: mpsc::Receiver<StreamMsg>) -> Self {
        Self { tx, rx }
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
        _cx: &mut Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(if self.tx.is_closed() {
            Err(CloudError::Reset(4))
        } else {
            Ok(())
        })
    }

    fn start_send(self: Pin<&mut Self>, item: StreamMsg) -> Result<(), Self::Error> {
        self.tx.try_send(item).map_err(|_| CloudError::Reset(4))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
}
