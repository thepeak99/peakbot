//! Log redaction (design §9.2 `logs.rs`, §4.9): TRACE logs must never contain
//! tokens, codes, verifiers, states, nonces, ID tokens, client secrets, the
//! subject, Pipe/letter payload markers, or card/sig bytes. Own test binary
//! (installs the global tracing subscriber). RED on purpose.

mod common;

use cloud::codec::ToRelay;

use common::*;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[tokio::test]
async fn logs_are_redacted() {
    let logs = LogCapture::install();
    let t = TestRelay::spawn().await;
    let sub = "log-sub-user-42";
    let flow = t.begin_login(DEFAULT_REDIRECT).await.expect("login 302");
    let cb = t
        .finish_login(&flow, &Mint { sub: Some(sub.into()), ..Default::default() })
        .await;
    let code = cb.code.expect("one-time code");
    let resp = t.get_token(&flow, &code).await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let v: serde_json::Value = resp.json().await.unwrap();
    let token = v["token"].as_str().unwrap().to_string();
    let id_token = t.idp.last_id_token().expect("id token minted");

    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut b = FakeDevice::new(2);
    let g = fetch_chain(&mut a, 0).await;
    let pipe_marker = "PBLOGPIPE-abcdef0123456789abcdef0123456789";
    let graw = cloud::chain::entry_hash(&g[0]);
    enroll_add(&t, &token, 1, graw, &mut a, &mut b).await;
    a.send(ToRelay::Pipe { to: b.id, bytes: pipe_marker.as_bytes().to_vec() }).await;
    match b.recv().await {
        cloud::codec::FromRelay::Pipe { .. } => {}
        other => panic!("expected Pipe, got {other:?}"),
    }
    let mut p = FakeDevice::new(3);
    p.connect(&t.ws_url(), &token).await;
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, Some(1), true).await;
    let letter_marker = "PBLOGLETTER-fedcba9876543210fedcba9876543210";
    post_to(&mut a, p.id, letter_marker.as_bytes().to_vec()).await;
    match p.recv().await {
        cloud::codec::FromRelay::Letter { .. } => {}
        other => panic!("expected Letter, got {other:?}"),
    }

    let buf = logs;
    for needle in [
        token.as_str(),
        code.as_str(),
        flow.device_verifier.as_str(),
        flow.device_state.as_str(),
        flow.relay_state.as_str(),
        flow.nonce.as_str(),
        id_token.as_str(),
        t.idp.client_secret.as_str(),
        sub,
        pipe_marker,
        letter_marker,
        &hex(&a.card),           // card bytes, hex
        &b64u(&a.card),           // card bytes, b64
        &hex(&b.card),
        &b64u(&b.card),
    ] {
        assert!(!buf.contains(needle), "log buffer must not contain {needle:?}");
    }
    // Positive control: the buffer is non-empty (the relay did log at TRACE).
    assert!(buf.contains("account"), "log buffer has account-related output");
}
