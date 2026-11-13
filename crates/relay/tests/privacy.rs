//! At-rest privacy (design §9.2 `privacy.rs`): nothing from Pipe ever reaches
//! the SQLite files; stored letter `sealed` does (positive control).
//! RED on purpose.

mod common;

use cloud::codec::{FromRelay, ToRelay};

use common::*;

fn marker() -> String {
    // PBMARK-<32 hex>
    format!("PBMARK-{:032x}", rand::random::<u128>() | (1u128 << 120))
}

fn file_contains(path: &std::path::Path, needle: &[u8]) -> bool {
    if !path.exists() {
        return false;
    }
    let bytes = std::fs::read(path).expect("read db file");
    bytes.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn db_contains_no_pipe_plaintext() {
    let mut t = TestRelay::spawn().await;
    let token = t.login("p1").await;
    let mut a = FakeDevice::new(1);
    enroll_genesis(&t, &token, &mut a).await;
    let mut b = FakeDevice::new(2);
    let g = fetch_chain(&mut a, 0).await;
    enroll_add(&t, &token, 1, cloud::chain::entry_hash(&g[0]), &mut a, &mut b).await;
    let mut c = FakeDevice::new(3);
    let g2 = fetch_chain(&mut a, 0).await;
    enroll_add(&t, &token, 2, cloud::chain::entry_hash(&g2[1]), &mut a, &mut c).await;

    let delivered = marker();
    let unreachable = marker();
    let stored = marker();

    // 1) A → B pipe carrying `delivered` (delivered live).
    a.send(ToRelay::Pipe { to: b.id, bytes: delivered.clone().into_bytes() }).await;
    match b.recv().await {
        FromRelay::Pipe { bytes, .. } => assert_eq!(String::from_utf8_lossy(&bytes), delivered),
        other => panic!("expected Pipe, got {other:?}"),
    }
    // 2) A → C pipe carrying `unreachable`: C goes offline first.
    c.close_code().await;
    a.send(ToRelay::Pipe { to: c.id, bytes: unreachable.clone().into_bytes() }).await;
    expect_unreachable(&a.recv().await, c.id).await;
    // 3) A letter to a pending device, `stored` inside the sealed ciphertext
    //    (positive control: the sealed letter IS stored).
    let mut p = FakeDevice::new(4);
    p.connect(&t.ws_url(), &token).await;
    p.hello_pending(&t.origin()).await;
    expect_welcome(&p.recv().await, Some(2), true).await;
    post_to(&mut a, p.id, stored.clone().into_bytes()).await;
    match p.recv().await {
        FromRelay::Letter { sealed, .. } => {
            assert_eq!(String::from_utf8_lossy(&sealed), stored)
        }
        other => panic!("expected Letter, got {other:?}"),
    }

    // Positive control: the stored sealed letter is on disk (pre-ack).
    let dir = t.tmp.path().to_path_buf();
    let mut db_files: Vec<std::path::PathBuf> = vec![dir.join("relay.db")];
    db_files.extend(["relay.db-wal", "relay.db-shm"].iter().map(|f| dir.join(f)));
    assert!(
        db_files.iter().any(|f| file_contains(f, stored.as_bytes())),
        "positive control: stored letter sealed is on disk"
    );

    // Shut down, then the pipe plaintext must be absent from every DB file.
    t.shutdown_all().await;
    db_files.extend(["relay.db", "relay.db-wal", "relay.db-shm"].iter().map(|f| dir.join(f)));
    for needle in [delivered.as_str(), unreachable.as_str()] {
        let n = needle.as_bytes();
        for f in &db_files {
            assert!(!file_contains(f, n), "pipe plaintext {needle} found in {}", f.display());
        }
    }
}
