//! The Local core ("PeakBot on this phone"): the app's only
//! stateful native piece. Binds loopback, boots `peakbot::embed::LocalCore`,
//! serves it, and restarts / re-serves it on demand.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use peakbot::embed::LocalCore;
use tauri::async_runtime::{self, JoinHandle};
use tauri_plugin_log::log;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use url::{Origin, Url};

/// the preferred port; busy ⇒ an ephemeral one.
const PORT: u16 = 47823;
/// `profile_open`'s budget for the core to answer HTTP.
const READY_WITHIN: Duration = Duration::from_secs(5);

/// The Local core, if running. `lifecycle` serialises start/restart/heal
/// across their awaits; `running` is only held for quick sync sections, so
/// the nav guard on Android's UI thread never waits on a boot.
#[derive(Default)]
pub struct Local {
    lifecycle: tokio::sync::Mutex<()>,
    running: std::sync::Mutex<Option<Running>>,
}

struct Running {
    core: LocalCore,
    port: u16,
    token: String,
    serve: JoinHandle<io::Result<()>>,
}

impl Local {
    /// The running core's origin — the only `http:` origin the webview may show.
    pub fn origin(&self) -> Option<Origin> {
        self.lock().as_ref().map(|r| origin(r.port))
    }

    pub fn is_running(&self) -> bool {
        self.lock().is_some()
    }

    /// Start the core if it isn't running (re-serving it if its listener
    /// died) and return the URL the webview opens — never handed to JS.
    pub async fn ensure(&self) -> Result<Url, String> {
        let _guard = self.lifecycle.lock().await;
        if self.is_running() {
            self.heal().await.map_err(start_error)?;
        } else {
            self.start(PORT).await.map_err(start_error)?;
        }
        Ok(self.url())
    }

    /// Drop the core and boot a fresh one from `config.yaml` (boot-only
    /// settings apply) on the same port. The token rotates: the old core's
    /// URL is dead the moment it stops, even if something kept a copy.
    pub async fn restart(&self) -> Result<Url, String> {
        let _guard = self.lifecycle.lock().await;
        let taken = self.lock().take();
        let port = match taken {
            Some(old) => old.stop().await,
            None => PORT,
        };
        self.start(port).await.map_err(start_error)?;
        Ok(self.url())
    }

    /// Foreground again: iOS may have reclaimed the listener while
    /// suspended. Returns the old origin and the new URL if the port moved.
    pub async fn resume(&self) -> Option<(Origin, Url)> {
        let _guard = self.lifecycle.lock().await;
        let before = self.origin()?;
        if let Err(e) = self.heal().await {
            log::warn!("local re-serve: {e}");
            return None;
        }
        let after = self.origin()?;
        (after != before).then(|| (before, self.url()))
    }

    /// Re-serve the same core on a fresh listener if the current one no
    /// longer accepts. Callers hold `lifecycle` and a core is running.
    async fn heal(&self) -> io::Result<()> {
        let port = self.lock().as_ref().expect("running").port;
        if TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .is_ok()
        {
            return Ok(());
        }
        log::info!("local listener on :{port} is gone, re-serving");
        let mut running = self.lock();
        let r = running.as_mut().expect("running");
        // A dead listener has already released the port, so it binds again.
        let listener = bind(r.port)?;
        r.serve.abort();
        r.port = listener.local_addr()?.port();
        r.serve = async_runtime::spawn(r.core.serve(listener));
        Ok(())
    }

    async fn start(&self, preferred: u16) -> anyhow::Result<()> {
        let listener = bind(preferred)?;
        let port = listener.local_addr()?.port();
        let token = new_token()?;
        let core = LocalCore::boot(token.clone()).await?;
        let serve = async_runtime::spawn(core.serve(listener));
        let running = Running {
            core,
            port,
            token,
            serve,
        };
        if !ready(port).await {
            running.stop().await;
            anyhow::bail!("it didn't answer within {} seconds", READY_WITHIN.as_secs());
        }
        log::info!("local core up on :{port}");
        *self.lock() = Some(running);
        Ok(())
    }

    fn url(&self) -> Url {
        let running = self.lock();
        let r = running.as_ref().expect("running");
        local_url(r.port, &r.token)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Running>> {
        self.running.lock().expect("local lock")
    }
}

impl Running {
    /// Stop serving, then drop the core; returns the port it was on. Waits
    /// for the aborted task so the listener is closed before a rebind.
    async fn stop(self) -> u16 {
        self.serve.abort();
        let _ = self.serve.await;
        drop(self.core);
        self.port
    }
}

fn start_error(e: impl std::fmt::Display) -> String {
    format!("Couldn't start PeakBot on this phone: {e}")
}

/// Loopback only: other apps on the phone can reach it, hence the
/// token. `SO_REUSEADDR` so a restart rebinds over the old core's TIME_WAITs.
fn bind(preferred: u16) -> io::Result<TcpListener> {
    let listen = |port: u16| {
        let socket = TcpSocket::new_v4()?;
        socket.set_reuseaddr(true)?;
        socket.bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))?;
        socket.listen(1024)
    };
    match listen(preferred) {
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            log::info!("port {preferred} is busy, using an ephemeral port");
            listen(0)
        }
        bound => bound,
    }
}

/// 128 bits from the OS RNG as 32 lowercase hex chars.
fn new_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn local_url(port: u16, token: &str) -> Url {
    let mut url = origin_url(port);
    url.query_pairs_mut().append_pair("token", token);
    url
}

fn origin(port: u16) -> Origin {
    origin_url(port).origin()
}

fn origin_url(port: u16) -> Url {
    Url::parse(&format!("http://127.0.0.1:{port}/")).expect("static url")
}

/// Restart gate: only a page on the Local origin may restart the core.
pub fn should_restart(current: &Url, local: Option<&Origin>) -> bool {
    local.is_some_and(|l| current.origin() == *l)
}

/// Poll until the core answers HTTP. No route is tokenless, so the bare
/// probe's expected answer is a 401 — any response means it's up.
async fn ready(port: u16) -> bool {
    let deadline = tokio::time::Instant::now() + READY_WITHIN;
    while tokio::time::Instant::now() < deadline {
        let head = tokio::time::timeout(Duration::from_millis(500), probe(port)).await;
        if let Ok(Ok(head)) = head
            && answered(&head)
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// The first bytes of the reply to a tokenless `GET /api/settings`.
async fn probe(port: u16) -> io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
    stream
        .write_all(b"GET /api/settings HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await?;
    let mut head = vec![0u8; 16];
    let n = stream.read(&mut head).await?;
    head.truncate(n);
    Ok(head)
}

/// Whether the bytes read back start an HTTP response's status line.
fn answered(head: &[u8]) -> bool {
    head.starts_with(b"HTTP/1.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn token_is_32_lowercase_hex_and_fresh_each_time() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_eq!(a.len(), 32);
        assert!(
            a.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')),
            "{a}"
        );
        assert_ne!(a, b);
    }

    #[test]
    fn local_url_carries_the_token_and_origin_does_not() {
        let url = local_url(47823, "ab12");
        assert_eq!(url.as_str(), "http://127.0.0.1:47823/?token=ab12");
        assert_eq!(
            origin(47823).ascii_serialization(),
            "http://127.0.0.1:47823"
        );
    }

    #[test]
    fn restart_gate_needs_the_current_page_on_the_local_origin() {
        let local = origin(47823);
        let gate = |current: &str, local| should_restart(&u(current), local);
        assert!(gate("http://127.0.0.1:47823/settings", Some(&local)));
        assert!(gate("http://127.0.0.1:47823/?token=x", Some(&local)));
        assert!(!gate("https://nas.lan:7823/", Some(&local)));
        assert!(!gate("http://127.0.0.1:9999/", Some(&local)));
        assert!(!gate("http://localhost:47823/", Some(&local)));
        assert!(!gate("http://tauri.localhost/", Some(&local)));
        assert!(!gate("http://127.0.0.1:47823/", None));
    }

    #[test]
    fn any_http_status_line_counts_as_answered() {
        assert!(answered(b"HTTP/1.1 401 Unauthorized"));
        assert!(answered(b"HTTP/1.1 200 OK\r\n"));
        assert!(answered(b"HTTP/1.0 503"));
        assert!(!answered(b""));
        assert!(!answered(b"HTTP"));
        assert!(!answered(b"SSH-2.0-OpenSSH"));
    }

    #[test]
    fn busy_port_falls_back_to_an_ephemeral_one() {
        async_runtime::block_on(async {
            let held = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let busy = held.local_addr().unwrap().port();
            let fallback = bind(busy).unwrap().local_addr().unwrap().port();
            assert_ne!(fallback, busy);
            assert_ne!(fallback, 0);

            drop(held);
            assert_eq!(bind(busy).unwrap().local_addr().unwrap().port(), busy);
        });
    }

    /// The whole lifecycle against a real core on a temp config root: the
    /// one test that calls `init_roots` (process-wide, boot-only).
    #[test]
    fn ensure_restart_and_heal_a_real_core() {
        let dir = std::env::temp_dir().join(format!("peakbot-mobile-local-{}", std::process::id()));
        peakbot::paths::init_roots(peakbot::paths::Roots {
            config: dir.join("config"),
            data: dir.join("core"),
            cache: dir.join("cache"),
        });
        let local = Local::default();
        async_runtime::block_on(async {
            assert!(local.origin().is_none());
            let first = local.ensure().await.unwrap();
            let port = first.port().unwrap();
            assert_eq!(local.origin(), Some(origin(port)));
            let head = probe(port).await.unwrap();
            assert!(head.starts_with(b"HTTP/1.1 401"), "{head:?}");
            assert_eq!(local.ensure().await.unwrap(), first, "running ⇒ same URL");

            let second = local.restart().await.unwrap();
            assert_eq!(second.port(), Some(port), "restart keeps the port");
            assert_ne!(second, first, "restart rotates the token");
            assert!(answered(&probe(port).await.unwrap()));

            // A reclaimed listener (iOS suspend): the same core is re-served.
            local.lock().as_ref().unwrap().serve.abort();
            while TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .is_ok()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(local.resume().await, None, "same port ⇒ nothing to move");
            assert!(answered(&probe(port).await.unwrap()));
            assert_eq!(local.ensure().await.unwrap(), second);
        });
        drop(local);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
