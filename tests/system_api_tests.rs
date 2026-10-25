//! T11 — System HTTP API tests (spec §8.2, 0.24 cutover).
//!
//! The setup wizard is gone: `GET/POST/DELETE /api/system/service` and
//! `POST /api/system/install` are the old `/api/setup/service` /
//! `/api/setup/install` handlers moved verbatim into
//! `peakbot::ui::web::system`, and the old `/api/setup*` surface answers
//! 404.
//!
//! Test strategy: spawn the real `system::router` (plus the
//! `setup_removed()` tombstones) on a random loopback port, attach the
//! token layer the same way production does, and drive it with
//! `reqwest`. This exercises actual TCP, the token layer, and the JSON
//! Content-Type gate.
//!
//! The install / service ops are fn-pointer seams. The fakes below
//! return canned `Result<JsonValue, SetupOpError>` values so the
//! *handler* (status mapping, JSON shape, error envelope) is exercised
//! without ever copying a binary or shelling out to `systemctl` — both
//! of which fail in the CI container (plan §E.9).

use axum::Router;
use axum::middleware::from_fn_with_state;
use peakbot::ui::web::require_token;
use peakbot::ui::web::system::{
    InstallFn, InstallPath, ServiceFn, SetupOpError, SystemState, router, setup_removed,
};
use std::net::SocketAddr;
use std::sync::Arc;

/// Spawn the system router (with the `/api/setup*` 404 tombstones)
/// behind the existing token layer on a random loopback port. The
/// install / service seams use the no-op `default_for_tests()` fakes.
async fn spawn_system(token: Option<&str>) -> SocketAddr {
    let state = SystemState {
        install: InstallFn::default_for_tests(),
        service: ServiceFn::default_for_tests(),
    };
    let mut app: Router = router(state).merge(setup_removed());
    if let Some(t) = token {
        let secret: Arc<str> = t.into();
        app = app.layer(from_fn_with_state(secret, require_token));
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

/// Spawn the system router with the install / service adapters set to
/// the test fakes. The fakes are process-global; the caller queues
/// their responses (via [`queue_install`] / [`queue_service`]) BEFORE
/// the request and the fakes read them in `spawn_blocking`.
async fn spawn_system_with_fakes(
    use_fake_install: bool,
    use_fake_service: bool,
    token: Option<&str>,
) -> SocketAddr {
    let state = SystemState {
        install: if use_fake_install {
            fake_install()
        } else {
            InstallFn::default_for_tests()
        },
        service: if use_fake_service {
            fake_service()
        } else {
            ServiceFn::default_for_tests()
        },
    };
    let mut app: Router = router(state).merge(setup_removed());
    if let Some(t) = token {
        let secret: Arc<str> = t.into();
        app = app.layer(from_fn_with_state(secret, require_token));
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    addr
}

/// Build a bare reqwest client that doesn't follow redirects.
fn bare_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

// ===========================================================================
// The fn-pointer seams cannot capture state. We work around this with
// a `SERIAL` lock + two `tokio::sync::Mutex` response cells:
//   * `SERIAL` — the test holds it for the entire test body, so
//     parallel tests cannot race for the response slot.
//   * `*_RESPONSE` — the dispatch (running in `spawn_blocking`) reads
//     the canned response from here.
// Because `SERIAL` is held by the test for the whole async body, two
// tests cannot overlap; the dispatch reads `*_RESPONSE` without
// touching `SERIAL`, so no deadlock.
// ===========================================================================

static SERIAL: std::sync::OnceLock<std::sync::Arc<tokio::sync::Mutex<()>>> =
    std::sync::OnceLock::new();
static INSTALL_RESPONSE: tokio::sync::Mutex<Option<Result<serde_json::Value, SetupOpError>>> =
    tokio::sync::Mutex::const_new(None);
static SERVICE_RESPONSE: tokio::sync::Mutex<Option<Result<serde_json::Value, SetupOpError>>> =
    tokio::sync::Mutex::const_new(None);

fn serial_lock() -> std::sync::Arc<tokio::sync::Mutex<()>> {
    SERIAL
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

async fn serial() -> tokio::sync::OwnedMutexGuard<()> {
    serial_lock().lock_owned().await
}

fn fake_install_dispatch(_req: serde_json::Value) -> Result<serde_json::Value, SetupOpError> {
    // The dispatch is called from `spawn_blocking`, so a sync mutex
    // is fine. `try_lock` because we cannot .await inside this fn —
    // the slot must already be populated by the test before the
    // handler invokes us. (If the test forgot to queue, we return a
    // 500 with a diagnostic.)
    match INSTALL_RESPONSE.try_lock() {
        Ok(mut g) => g.take().unwrap_or_else(|| {
            Err(SetupOpError::internal(
                "no install fake queued (test forgot to call queue_install)",
                Vec::new(),
            ))
        }),
        Err(_) => Err(SetupOpError::internal(
            "install fake mutex contention",
            Vec::new(),
        )),
    }
}

fn fake_service_dispatch(_req: serde_json::Value) -> Result<serde_json::Value, SetupOpError> {
    match SERVICE_RESPONSE.try_lock() {
        Ok(mut g) => g.take().unwrap_or_else(|| {
            Err(SetupOpError::internal(
                "no service fake queued (test forgot to call queue_service)",
                Vec::new(),
            ))
        }),
        Err(_) => Err(SetupOpError::internal(
            "service fake mutex contention",
            Vec::new(),
        )),
    }
}

fn fake_install() -> InstallFn {
    InstallFn(fake_install_dispatch)
}
fn fake_service() -> ServiceFn {
    ServiceFn(fake_service_dispatch)
}

/// Park a response on the install fake. The serial guard from
/// `serial()` must already be held so the slot is private to this
/// test; the dispatch will `try_lock` + take the response exactly
/// once.
fn queue_install(response: Result<serde_json::Value, SetupOpError>) {
    *INSTALL_RESPONSE
        .try_lock()
        .expect("install cell not writable under serial") = Some(response);
}
fn queue_service(response: Result<serde_json::Value, SetupOpError>) {
    *SERVICE_RESPONSE
        .try_lock()
        .expect("service cell not writable under serial") = Some(response);
}

// ── InstallPath wire shape (pure data) ──────────────────────────────────

#[tokio::test]
async fn install_path_from_core_round_trips_all_variants() {
    // The in-module `InstallPath::from_core` is the only place the
    // core enum is converted to wire JSON. Exercise every variant
    // here so the install response can't silently drop a case.
    use peakbot::install::PathState;
    let on = InstallPath::from_core(&PathState::OnPath);
    let sh = InstallPath::from_core(&PathState::Shadowed {
        by: std::path::PathBuf::from("/opt/other/peakbot"),
    });
    let ab = InstallPath::from_core(&PathState::NotOnPath {
        hint: "add me".to_string(),
    });
    let v_on = serde_json::to_value(&on).unwrap();
    let v_sh = serde_json::to_value(&sh).unwrap();
    let v_ab = serde_json::to_value(&ab).unwrap();
    assert_eq!(v_on["status"], "on_path");
    assert_eq!(v_sh["status"], "shadowed");
    assert_eq!(v_sh["by"], "/opt/other/peakbot");
    assert_eq!(v_ab["status"], "absent");
    assert_eq!(v_ab["hint"], "add me");
}

// ── POST /api/system/install ────────────────────────────────────────────

#[tokio::test]
async fn post_install_success_returns_200_with_full_wire_shape() {
    let _serial = serial().await;
    queue_install(Ok(serde_json::json!({
        "source": "/x/peakbot",
        "target": "/home/u/.local/bin/peakbot",
        "action": "installed",
        "path": {"status": "on_path"},
        "notes": ["restart me"]
    })));
    let addr = spawn_system_with_fakes(true, false, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/system/install"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["action"], "installed");
    assert_eq!(body["source"], "/x/peakbot");
    assert_eq!(body["target"], "/home/u/.local/bin/peakbot");
    assert_eq!(body["path"]["status"], "on_path");
    assert_eq!(body["notes"][0], "restart me");
}

#[tokio::test]
async fn post_install_unsupported_maps_to_501() {
    let _serial = serial().await;
    queue_install(Err(SetupOpError::unsupported("no systemd here")));
    let addr = spawn_system_with_fakes(true, false, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/system/install"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 501, "Unsupported must map to 501 per §B");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "no systemd here");
}

#[tokio::test]
async fn post_install_io_error_maps_to_500() {
    let _serial = serial().await;
    queue_install(Err(SetupOpError::internal(
        "install I/O error: permission denied",
        Vec::new(),
    )));
    let addr = spawn_system_with_fakes(true, false, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/system/install"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("install I/O error"),
        "500 must surface the underlying error message verbatim"
    );
}

#[tokio::test]
async fn post_install_command_failed_maps_to_500_with_problems() {
    let _serial = serial().await;
    // §B: a CommandFailed-like failure must surface stderr in `problems`.
    queue_install(Err(SetupOpError::internal(
        "`systemctl --user enable` failed: bus not reachable",
        vec!["bus not reachable".to_string()],
    )));
    let addr = spawn_system_with_fakes(true, false, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/system/install"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.unwrap();
    let problems = body
        .get("problems")
        .and_then(|p| p.as_array())
        .expect("problems[]");
    assert_eq!(problems[0], "bus not reachable");
}

// ── GET|POST|DELETE /api/system/service ─────────────────────────────────

#[tokio::test]
async fn get_service_status_returns_200_with_service_report_shape() {
    let _serial = serial().await;
    queue_service(Ok(serde_json::json!({
        "manager": "systemd-user",
        "name": "peakbot.service",
        "artifact": "/home/u/.config/systemd/user/peakbot.service",
        "installed": true,
        "exe": "/home/u/.local/bin/peakbot",
        "run_state": "running",
        "tls": false,
        "survives_logout": false,
        "commands": ["systemctl --user is-active peakbot.service"],
        "notes": ["linger is off"]
    })));
    let addr = spawn_system_with_fakes(false, true, None).await;

    let resp = bare_client()
        .get(format!("http://{addr}/api/system/service"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    // §B field-for-field: every key must be present.
    for key in [
        "manager",
        "name",
        "artifact",
        "installed",
        "exe",
        "run_state",
        "tls",
        "survives_logout",
        "commands",
        "notes",
    ] {
        assert!(
            body.get(key).is_some(),
            "ServiceResponse missing field {key}"
        );
    }
    assert_eq!(body["manager"], "systemd-user");
    assert_eq!(body["run_state"], "running");
}

#[tokio::test]
async fn post_service_install_returns_200_with_bind_and_token_passed_through() {
    let _serial = serial().await;
    queue_service(Ok(serde_json::json!({
        "manager": "systemd-user",
        "name": "peakbot.service",
        "artifact": "/home/u/.config/systemd/user/peakbot.service",
        "installed": true,
        "exe": "/home/u/.local/bin/peakbot",
        "run_state": "unknown",
        "tls": false,
        "survives_logout": false,
        "commands": [],
        "notes": []
    })));
    let addr = spawn_system_with_fakes(false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/system/service"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            serde_json::json!({
                "op": "install",
                "bind": "127.0.0.1:7823",
                "token": "s3cret",
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["installed"], true);
}

#[tokio::test]
async fn post_service_lan_without_token_maps_to_422() {
    let _serial = serial().await;
    // Adapter surfaces 422 on PlanError::TokenRequired; the fake
    // returns one to confirm the handler passes it through unchanged.
    queue_service(Err(SetupOpError::validation(
        "refusing to plan a service on non-loopback 0.0.0.0:7823: a token is required \
         (set --token or PEAKBOT_WEB_TOKEN)",
    )));
    let addr = spawn_system_with_fakes(false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/system/service"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({"op":"install", "bind":"0.0.0.0:7823"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        422,
        "PlanError::TokenRequired must map to 422 per §B"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let msg = body["error"].as_str().unwrap_or("");
    assert!(
        msg.contains("0.0.0.0:7823") && msg.contains("token"),
        "422 must echo the rejected bind and name the missing token; got: {msg}"
    );
}

#[tokio::test]
async fn post_service_unsupported_maps_to_501() {
    let _serial = serial().await;
    queue_service(Err(SetupOpError::unsupported(
        "no service manager on this platform",
    )));
    let addr = spawn_system_with_fakes(false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/system/service"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({"op":"install"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 501);
}

#[tokio::test]
async fn delete_service_returns_200_with_notes_passed_through() {
    let _serial = serial().await;
    // The handler synthesises `{"op":"uninstall"}` and the fake
    // returns a pre-shaped report. The 0.24 adapter no longer appends
    // a web-token note (the token file is retired with the 0.24
    // layout), so the notes are the platform module's verbatim.
    queue_service(Ok(serde_json::json!({
        "manager": "systemd-user",
        "name": "peakbot.service",
        "artifact": null,
        "installed": false,
        "exe": null,
        "run_state": "unknown",
        "tls": false,
        "survives_logout": false,
        "commands": ["systemctl --user disable --now peakbot.service"],
        "notes": ["service unit removed"]
    })));
    let addr = spawn_system_with_fakes(false, true, None).await;

    let resp = bare_client()
        .delete(format!("http://{addr}/api/system/service"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let notes: Vec<String> = body["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_string())
        .collect();
    assert!(
        notes.iter().any(|n| n.contains("service unit removed")),
        "delete response must carry the service report notes; got: {notes:?}"
    );
}

#[tokio::test]
async fn post_service_unknown_op_maps_to_422() {
    let _serial = serial().await;
    // The fake stays queued unused — the handler validates first.
    queue_service(Ok(serde_json::json!({})));
    let addr = spawn_system_with_fakes(false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/system/service"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::json!({"op": "freeze"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("unknown service op")
    );
}

#[tokio::test]
async fn post_service_missing_op_defaults_to_install_with_200() {
    // §B: POST body is `{bind?, token?}` — no `op` field. The
    // handler synthesises `op:"install"` and the install runs, so
    // missing `op` is *not* an error. The fake returns success.
    let _serial = serial().await;
    queue_service(Ok(serde_json::json!({
        "manager": "systemd-user",
        "name": "peakbot.service",
        "artifact": null,
        "installed": false,
        "exe": null,
        "run_state": "unknown",
        "tls": false,
        "survives_logout": false,
        "commands": [],
        "notes": []
    })));
    let addr = spawn_system_with_fakes(false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/system/service"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "POST with no `op` defaults to install per §B"
    );
}

#[tokio::test]
async fn post_service_with_wrong_content_type_returns_415() {
    let _serial = serial().await;
    queue_service(Ok(serde_json::json!({})));
    let addr = spawn_system_with_fakes(false, true, None).await;

    let resp = bare_client()
        .post(format!("http://{addr}/api/system/service"))
        .header(reqwest::header::CONTENT_TYPE, "text/plain")
        .body("hello")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        415,
        "POST /api/system/service must reject non-JSON"
    );
}

// ── Token gating + the removed /api/setup surface ───────────────────────

#[tokio::test]
async fn system_routes_require_token_when_one_is_configured() {
    let _serial = serial().await;
    queue_install(Ok(serde_json::json!({})));
    queue_service(Ok(serde_json::json!({})));
    let addr = spawn_system_with_fakes(true, true, Some("s3cret")).await;

    // /api/system/service — every verb 401s without the token.
    for verb in ["get", "post", "delete"] {
        let url = format!("http://{addr}/api/system/service");
        let req = match verb {
            "get" => bare_client().get(url).send(),
            "post" => bare_client()
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body("{}")
                .send(),
            _ => bare_client().delete(url).send(),
        };
        let resp = req.await.unwrap();
        assert_eq!(
            resp.status(),
            401,
            "{verb} /api/system/service must 401 without token"
        );
    }

    // /api/system/install — 401 without the token.
    let resp = bare_client()
        .post(format!("http://{addr}/api/system/install"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "POST /api/system/install must 401 without token"
    );
}

#[tokio::test]
async fn api_setup_is_404() {
    // The setup wizard is gone (0.24 cutover): every route of the old
    // `/api/setup` surface answers 404 with the ApiError envelope, not
    // the SPA fallback's index.html.
    let addr = spawn_system(None).await;

    let cases: [(&str, &str); 5] = [
        ("get", "/api/setup"),
        ("post", "/api/setup/config"),
        ("post", "/api/setup/install"),
        ("post", "/api/setup/service"),
        ("delete", "/api/setup/service"),
    ];
    for (verb, path) in cases {
        let url = format!("http://{addr}{path}");
        let req = match verb {
            "get" => bare_client().get(url).send(),
            "post" => bare_client()
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body("{}")
                .send(),
            _ => bare_client().delete(url).send(),
        };
        let resp = req.await.unwrap();
        assert_eq!(resp.status(), 404, "{verb} {path} must be a 404 tombstone");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body.get("error").is_some(),
            "{verb} {path} must use the ApiError envelope; got: {body}"
        );
    }
}
