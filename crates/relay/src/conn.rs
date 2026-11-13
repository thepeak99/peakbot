//! One task per WebSocket: the §4.2 state machine (Authed → Active/Pending),
//! the Hello window, both Hellos (+ session re-check), Pipe/Post/Ack/
//! ChainFetch/ChainAppend/EnrollGenesis, the post window, the outbound writer
//! and the `close` select (design §4.2–§4.6, §6.2).
//!
//! SKELETON: signature is the locked interface; body is `todo!()`.

use cloud::DeviceId;

use crate::hub::{Conn, Hub};
use crate::store::Db;

/// Run the per-socket state machine until the socket closes.
pub async fn run_conn(conn: Conn, device: Option<DeviceId>, hub: Hub, db: Db, origin: String) {
    let _ = (conn, device, hub, db, origin);
    todo!("P2 task 8: the §4.2 state machine")
}
