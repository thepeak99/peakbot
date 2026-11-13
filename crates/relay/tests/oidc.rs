#![allow(dead_code)] // shared harness: each test binary uses a slice of it

//! OIDC login tests (design §9.2 `oidc.rs`). RED on purpose: the relay is a
//! `todo!()` skeleton; the harness (FakeIdp + login dance) is real.

mod common;

use common::*;

/// Every `id_token_*` case ends the same way: the callback 302s to the
/// device's redirect with `error=access_denied`, and the DB gets neither an
/// `accounts` row nor a `sessions` row (design §9.2).
async fn assert_access_denied(t: &TestRelay, mint: &Mint) {
    let flow = t
        .begin_login(DEFAULT_REDIRECT)
        .await
        .expect("GET /login → 302");
    let cb = t.finish_login(&flow, mint).await;
    assert_eq!(
        cb.response.status(),
        axum::http::StatusCode::FOUND,
        "callback → 302"
    );
    assert_eq!(
        cb.error.as_deref(),
        Some("access_denied"),
        "error=access_denied"
    );
    assert!(cb.code.is_none(), "no one-time code on failure");
    assert_eq!(
        cb.device_state.as_deref(),
        Some("dev-state-1"),
        "device state echoed"
    );
}

#[tokio::test]
async fn login_happy_path_es256() {
    let t = TestRelay::spawn().await;
    let token = t.login("user-es256").await;
    assert!(!token.is_empty());
    // A second login for the same sub is the same account (login is idempotent).
    let again = t.login("user-es256").await;
    assert!(!again.is_empty());
}

#[tokio::test]
async fn login_happy_path_rs256() {
    let t = TestRelay::spawn_idp(FakeIdp::spawn(true).await).await;
    let flow = t.begin_login(DEFAULT_REDIRECT).await.expect("login 302");
    let cb = t
        .finish_login(
            &flow,
            &Mint {
                alg: Some(IdpAlg::Rs256),
                sub: Some("user-rs256".into()),
                ..Default::default()
            },
        )
        .await;
    assert_eq!(cb.error.as_deref(), None, "callback ok");
    assert!(cb.code.is_some());
    let resp = t.get_token(&flow, cb.code.as_ref().unwrap()).await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert!(!v["token"].as_str().unwrap().is_empty());
}

macro_rules! rejected_redirect_uri {
    ($name:ident, $uri:expr) => {
        #[tokio::test]
        async fn $name() {
            let t = TestRelay::spawn().await;
            let resp = match t.begin_login($uri).await {
                Ok(_) => panic!("redirect_uri {} must be rejected", $uri),
                Err(resp) => resp,
            };
            assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST, "400");
            assert!(
                resp.headers().get(axum::http::header::LOCATION).is_none(),
                "no Location on 400"
            );
        }
    };
}

rejected_redirect_uri!(redirect_uri_rejected_localhost, "http://localhost:8080/cb");
rejected_redirect_uri!(redirect_uri_rejected_ipv6_loopback, "http://[::1]:8080/cb");
rejected_redirect_uri!(redirect_uri_rejected_https, "https://127.0.0.1:8080/cb");
rejected_redirect_uri!(redirect_uri_rejected_missing_port, "http://127.0.0.1/cb");
rejected_redirect_uri!(redirect_uri_rejected_other_host, "http://10.0.0.5:8080/cb");
rejected_redirect_uri!(
    redirect_uri_rejected_userinfo,
    "http://user:pass@127.0.0.1:8080/cb"
);
rejected_redirect_uri!(
    redirect_uri_rejected_fragment,
    "http://127.0.0.1:8080/cb#frag"
);
rejected_redirect_uri!(redirect_uri_rejected_javascript, "javascript:alert(1)");
rejected_redirect_uri!(
    redirect_uri_rejected_unconfigured_app_scheme,
    "com.peakbot.app:/cb"
);

#[tokio::test]
async fn redirect_uri_loopback_query_preserved() {
    let t = TestRelay::spawn().await;
    let uri = "http://127.0.0.1:9/cb?x=1&y=2";
    let flow = t.begin_login(uri).await.expect("login 302");
    let cb = t
        .finish_login(
            &flow,
            &Mint {
                sub: Some("q".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(cb.code.is_some(), "login succeeds");
    // The relay appends its own params and preserves the device query.
    let loc = cb
        .response
        .headers()
        .get(axum::http::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    let u = url::Url::parse(loc).unwrap();
    let qp: std::collections::HashMap<String, String> = u.query_pairs().into_owned().collect();
    assert_eq!(
        qp.get("x").map(String::as_str),
        Some("1"),
        "query preserved"
    );
    assert_eq!(
        qp.get("y").map(String::as_str),
        Some("2"),
        "query preserved"
    );
    assert!(qp.contains_key("code"), "relay appends code");
    assert_eq!(qp.get("state").map(String::as_str), Some("dev-state-1"));
}

#[tokio::test]
async fn id_token_wrong_aud() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            aud: Some(Aud::Str("other-client".into())),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_multi_aud_no_azp() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            aud: Some(Aud::Vec(vec!["relay-client-id".into(), "other".into()])),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_azp_mismatch() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            azp: Some(Some("other-client".into())),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_wrong_iss() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            iss: Some("http://evil.example".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_expired_beyond_skew() {
    let t = TestRelay::spawn().await;
    // exp = now − 3600 − 3600 + 3600 = now − 3600 ⇒ far beyond the 60 s skew.
    assert_access_denied(
        &t,
        &Mint {
            exp_offset: -7200,
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_iat_in_future() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            iat_offset: 120,
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_nonce_mismatch() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            nonce: Some(Some("wrong-nonce".into())),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_nonce_missing() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            nonce: Some(None),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_bad_signature() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            alt_key: true,
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_alg_none() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            alg: Some(IdpAlg::None),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_hs256_with_client_secret() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            alg: Some(IdpAlg::Hs256),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_unknown_kid() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            kid: Some("ghost-kid".into()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_empty_sub() {
    let t = TestRelay::spawn().await;
    assert_access_denied(
        &t,
        &Mint {
            sub: Some(String::new()),
            ..Default::default()
        },
    )
    .await;
}

#[tokio::test]
async fn id_token_exp_within_skew_accepted() {
    let t = TestRelay::spawn().await;
    // exp = now − 30, inside the 60 s leeway ⇒ accepted (design §9.2).
    let flow = t.begin_login(DEFAULT_REDIRECT).await.expect("login 302");
    let cb = t
        .finish_login(
            &flow,
            &Mint {
                exp_offset: -3630, // exp = now + 3600 − 3630 = now − 30 (inside 60 s skew)
                sub: Some("skew".into()),
                ..Default::default()
            },
        )
        .await;
    assert!(
        cb.code.is_some(),
        "exp within skew is accepted, got error={:?}",
        cb.error
    );
    let resp = t.get_token(&flow, cb.code.as_ref().unwrap()).await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
}

#[tokio::test]
async fn discovery_issuer_mismatch_rejected() {
    let t = TestRelay::spawn().await;
    t.idp
        .set_discovery_issuer(Some("http://not-the-configured-issuer".into()));
    // The relay discovers fresh at /login; a mismatching doc.issuer must fail.
    let resp = match t.begin_login(DEFAULT_REDIRECT).await {
        Ok(_) => panic!("mismatched discovery issuer must be rejected"),
        Err(resp) => resp,
    };
    assert!(
        resp.status() == axum::http::StatusCode::BAD_REQUEST
            || resp.status() == axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "400/500, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn callback_state_single_use() {
    let t = TestRelay::spawn().await;
    let flow = t.begin_login(DEFAULT_REDIRECT).await.expect("login 302");
    let code = t.idp.issue_code(&flow.nonce, &flow.challenge);
    // First callback: ok.
    let r1 = t.http_get_callback(&code, &flow.relay_state).await;
    assert_eq!(r1.status(), axum::http::StatusCode::FOUND);
    // Second callback with the same state: the flow is gone → 400, no redirect.
    let r2 = t.http_get_callback(&code, &flow.relay_state).await;
    assert_eq!(
        r2.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "state single use"
    );
    assert!(r2.headers().get(axum::http::header::LOCATION).is_none());
}

#[tokio::test]
async fn pkce_verifier_mismatch_invalid_grant() {
    let t = TestRelay::spawn().await;
    let flow = t.begin_login(DEFAULT_REDIRECT).await.expect("login 302");
    let cb = t
        .finish_login(
            &flow,
            &Mint {
                sub: Some("pkce".into()),
                ..Default::default()
            },
        )
        .await;
    let code = cb.code.expect("one-time code");
    // Wrong verifier → 400 invalid_grant.
    let mut bad = flow.clone();
    bad.device_verifier = b64u(&rand_bytes(32));
    let r1 = t.get_token(&bad, &code).await;
    assert_eq!(r1.status(), axum::http::StatusCode::BAD_REQUEST);
    let v: serde_json::Value = r1.json().await.unwrap();
    assert_eq!(v["error"].as_str(), Some("invalid_grant"));
    // The grant is consumed on failure: the right verifier now also gets 400.
    let r2 = t.get_token(&flow, &code).await;
    assert_eq!(
        r2.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "grant consumed"
    );
    let v: serde_json::Value = r2.json().await.unwrap();
    assert_eq!(v["error"].as_str(), Some("invalid_grant"));
}

#[tokio::test]
async fn one_time_code_reuse_invalid_grant() {
    let t = TestRelay::spawn().await;
    let flow = t.begin_login(DEFAULT_REDIRECT).await.expect("login 302");
    let cb = t
        .finish_login(
            &flow,
            &Mint {
                sub: Some("reuse".into()),
                ..Default::default()
            },
        )
        .await;
    let code = cb.code.expect("one-time code");
    let r1 = t.get_token(&flow, &code).await;
    assert_eq!(r1.status(), axum::http::StatusCode::OK, "first use ok");
    let r2 = t.get_token(&flow, &code).await;
    assert_eq!(
        r2.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "reuse → 400"
    );
    let v: serde_json::Value = r2.json().await.unwrap();
    assert_eq!(v["error"].as_str(), Some("invalid_grant"));
}
