//! Cloud Transport v2 framing — pure tests of the new StreamMsg variants
//! (FrameBegin, FrameEnd, Credit), the fragment assembler, and the credit
//! ledger. These are COMPILE-RED until the implementer adds the variants
//! to the StreamMsg enum.
//!
//! Contract under test:
//! 1. FrameBegin{len} + Chunk* + FrameEnd reassembles to exact UTF-8 text.
//! 2. The assembler rejects malformed fragment sequences.
//! 3. MAX_LOGICAL_MESSAGE (64 MiB) is enforced before allocation.
//! 4. The credit ledger never goes negative and never exceeds its max.
//! 5. FrameBegin/FrameEnd/Credit are forbidden on non-Attach streams.

use cloud::Error;
use cloud::frames::{Inner, StreamMsg};

/// Reassemble a FrameBegin+Chunks+FrameEnd sequence into the original text.
fn assemble(begin_len: u32, chunks: &[Vec<u8>], end: bool) -> Result<String, Error> {
    // Feed FrameBegin to the assembler.
    let begin = StreamMsg::FrameBegin { len: begin_len };
    let mut assembler = cloud::frames::FragmentAssembler::new();
    assembler.feed(begin)?;

    // Feed all Chunks.
    for c in chunks {
        assembler.feed(StreamMsg::Chunk(c.clone()))?;
    }

    // Optionally feed FrameEnd; it must produce the completed message.
    if end {
        return assembler.feed(StreamMsg::FrameEnd)?.ok_or(Error::Protocol);
    }

    Err(Error::Protocol)
}

// ── §1: happy path reassembly ───────────────────────────────────────────────

/// A single small Frame (not fragmented) should work as before: no
/// FrameBegin/End, just Frame. This is a regression test for existing
/// functionality that must not break.
#[test]
fn assembler_small_message_single_frame() {
    let msg = StreamMsg::Frame("hello".into());
    let inner = Inner { stream: 1, msg };
    let bytes = cloud::frames::encode_inner(&inner);
    let decoded = cloud::frames::decode_inner(&bytes).expect("decode");
    assert_eq!(decoded.msg, StreamMsg::Frame("hello".into()));
}

/// A payload that exactly spans chunk boundaries: 128 KiB with 64 KiB chunks
/// (two chunks, zero remainder). The assembler must not allocate 64 MiB
/// up front and must not lose the final bytes.
#[test]
fn assembler_exact_chunk_boundary() {
    let payload = "X".repeat(128 * 1024);
    let bytes = payload.as_bytes().to_vec();
    let chunks: Vec<Vec<u8>> = bytes.chunks(64 * 1024).map(|c| c.to_vec()).collect();
    let got = assemble(payload.len() as u32, &chunks, true);
    assert_eq!(got, Ok(payload));
}

/// A payload with multibyte UTF-8 (Chinese, emoji, Latin-1) where chunk
/// boundaries fall in the middle of codepoints. The assembler is byte-based:
/// it must not split codepoints at chunk boundaries and must deliver exact
/// UTF-8. 3-byte chunks guarantee every 4-byte codepoint is split.
#[test]
fn assembler_multibyte_utf8_preserved() {
    let payload = "Hello 🚀 你好 мир Привет مرحبا Καλημέρα 안녕하세요 Bonjour 👋".repeat(1000);
    let bytes = payload.as_bytes().to_vec();
    let chunks: Vec<Vec<u8>> = bytes.chunks(3).map(|c| c.to_vec()).collect();
    let got = assemble(payload.len() as u32, &chunks, true);
    assert_eq!(got, Ok(payload), "reassembly must produce exact UTF-8");
}

/// A payload slightly over 1 MiB — the current MAX_INNER limit — proves
/// the new framing can transport what the old framing could not.
#[test]
fn assembler_over_current_max_inner() {
    let payload = "Y".repeat(2 * 1024 * 1024);
    let bytes = payload.as_bytes().to_vec();
    let chunks: Vec<Vec<u8>> = bytes.chunks(64 * 1024).map(|c| c.to_vec()).collect();
    let got = assemble(payload.len() as u32, &chunks, true);
    assert_eq!(got, Ok(payload));
}

// ── §2: malformed fragment sequences ────────────────────────────────────────

/// Chunk without a preceding FrameBegin must be rejected as Protocol.
#[test]
fn assembler_rejects_chunk_without_begin() {
    let mut assembler = cloud::frames::FragmentAssembler::new();
    let r = assembler.feed(StreamMsg::Chunk(vec![1, 2, 3]));
    assert_eq!(r, Err(Error::Protocol));
}

/// Two FrameBegin messages back-to-back without an intervening FrameEnd.
#[test]
fn assembler_rejects_nested_begin() {
    let mut assembler = cloud::frames::FragmentAssembler::new();
    assembler
        .feed(StreamMsg::FrameBegin { len: 1 })
        .expect("first begin");
    assembler.feed(StreamMsg::Chunk(vec![1])).expect("chunk");
    let r = assembler.feed(StreamMsg::FrameBegin { len: 2 });
    assert_eq!(r, Err(Error::Protocol));
}

/// FrameEnd without a preceding FrameBegin.
#[test]
fn assembler_rejects_end_without_begin() {
    let mut assembler = cloud::frames::FragmentAssembler::new();
    let r = assembler.feed(StreamMsg::FrameEnd);
    assert_eq!(r, Err(Error::Protocol));
}

/// The declared length is larger than the actual data sent.
#[test]
fn assembler_rejects_length_underflow() {
    let payload = "A".repeat(100);
    let chunks = vec![payload.as_bytes().to_vec()];
    // Declare 200 bytes but send only 100.
    let r = assemble(200, &chunks, true);
    assert_eq!(r, Err(Error::Protocol));
}

/// The declared length is smaller than the actual data sent.
#[test]
fn assembler_rejects_length_overflow() {
    let payload = "B".repeat(200);
    let chunks = vec![payload.as_bytes().to_vec()];
    // Declare 100 bytes but send 200.
    let r = assemble(100, &chunks, true);
    assert_eq!(r, Err(Error::Protocol));
}

/// The reassembled bytes are not valid UTF-8.
#[test]
fn assembler_rejects_invalid_utf8() {
    let chunks = vec![vec![0xFF, 0xFF, 0xFF]];
    let r = assemble(3, &chunks, true);
    assert_eq!(r, Err(Error::Protocol));
}

/// FrameEnd arrives mid-message (before all declared bytes).
#[test]
fn assembler_rejects_end_mid_message() {
    let payload = "C".repeat(200);
    let chunks = vec![payload.as_bytes().to_vec()];
    // Begin declares 400 bytes, send 200, end.
    let r = assemble(400, &chunks, true);
    assert_eq!(r, Err(Error::Protocol));
}

// ── §3: size limits ─────────────────────────────────────────────────────────

/// A message larger than MAX_LOGICAL_MESSAGE (64 MiB) is rejected before
/// the assembler tries to allocate it.
#[test]
fn assembler_rejects_over_64mib_before_allocation() {
    let big = (64 * 1024 * 1024) + 1;
    // FrameBegin declares 64 MiB + 1.
    let begin = StreamMsg::FrameBegin { len: big as u32 };
    let mut assembler = cloud::frames::FragmentAssembler::new();
    let r = assembler.feed(begin);
    assert_eq!(r, Err(Error::Protocol));
}

/// A chunk exactly MAX_CHUNK is accepted; one byte more is rejected.
#[test]
fn assembler_chunk_size_boundary() {
    let max = cloud::constants::MAX_CHUNK;
    let payload = vec![b'Z'; max + 1];
    let chunks: Vec<Vec<u8>> = payload.chunks(max).map(|c| c.to_vec()).collect();
    // First chunk is exactly MAX_CHUNK; the second one byte.
    let got = assemble(payload.len() as u32, &chunks, true);
    assert!(got.is_ok(), "MAX_CHUNK-sized chunks are legal");

    let mut assembler = cloud::frames::FragmentAssembler::new();
    assembler
        .feed(StreamMsg::FrameBegin {
            len: (max + 1) as u32,
        })
        .expect("begin");
    let r = assembler.feed(StreamMsg::Chunk(vec![0u8; max + 1]));
    assert_eq!(r, Err(Error::Protocol), "a chunk over MAX_CHUNK aborts");
}

/// The one-shot Frame threshold must leave room for framing inside the
/// inner-record and pipe-envelope caps.
#[test]
fn one_shot_threshold_fits_one_inner_record() {
    let payload = "x".repeat(cloud::constants::FRAME_ONESHOT_MAX);
    let inner = Inner {
        stream: 1,
        msg: StreamMsg::Frame(payload),
    };
    let bytes = cloud::frames::encode_inner(&inner);
    assert!(
        bytes.len() <= cloud::constants::MAX_INNER,
        "a one-shot Frame must fit MAX_INNER"
    );
    assert!(
        bytes.len() <= cloud::constants::MAX_ENVELOPE_PIPE,
        "a one-shot Frame must fit the pipe envelope"
    );
    assert!(
        cloud::constants::FRAME_ONESHOT_MAX <= cloud::constants::CREDIT_INITIAL as usize,
        "a one-shot Frame must fit the initial credit window"
    );
}

// ── §4: credit ledger ───────────────────────────────────────────────────────

/// The initial credit is 512 KiB (2^19 bytes).
#[test]
fn credit_initial_512kib() {
    let ledger = cloud::frames::CreditLedger::new(512 * 1024, 512 * 1024);
    assert_eq!(ledger.available(), 512 * 1024);
}

/// Consuming more than the available credit fails.
#[test]
fn credit_never_negative() {
    let mut ledger = cloud::frames::CreditLedger::new(1000, 1000);
    ledger.consume(600).expect("consume");
    let r = ledger.consume(500);
    assert_eq!(r, Err(Error::Credit));
}

/// Grants cannot push the ledger above its max.
#[test]
fn credit_never_over_max() {
    let mut ledger = cloud::frames::CreditLedger::new(1000, 1000);
    ledger.consume(500).expect("consume");
    ledger.grant(800).expect("grant");
    assert_eq!(ledger.available(), 1000, "capped at max");
}

/// Granting exactly the consumed amount returns the ledger to full.
#[test]
fn credit_grant_cadence_256kib() {
    let max = 512 * 1024;
    let grant = 256 * 1024;
    let mut ledger = cloud::frames::CreditLedger::new(max, max);
    ledger.consume(grant).expect("consume");
    assert_eq!(ledger.available(), grant);
    ledger.grant(grant).expect("grant");
    assert_eq!(ledger.available(), max, "regranted to full");
}

/// A zero-byte grant is rejected.
#[test]
fn credit_zero_grant_rejected() {
    let mut ledger = cloud::frames::CreditLedger::new(1000, 1000);
    let r = ledger.grant(0);
    assert_eq!(r, Err(Error::Protocol));
}

/// A grant larger than the initial credit is clamped to fill the ledger.
#[test]
fn credit_oversize_grant_clamped() {
    let max = 1024;
    let mut ledger = cloud::frames::CreditLedger::new(max, max);
    ledger.consume(500).expect("consume");
    ledger.grant(2048).expect("grant");
    assert_eq!(ledger.available(), max, "clamped to max");
}

// ── §4b: reassembly budget ──────────────────────────────────────────────────

/// Two fragmented messages may not reserve more than the per-session
/// aggregate; the second Begin is rejected before any allocation.
#[test]
fn assembler_session_budget_rejects_aggregate_over_64mib() {
    let budget = cloud::frames::ReassemblyBudget::new(64 * 1024 * 1024);
    let mut first = cloud::frames::FragmentAssembler::with_budget(budget.clone());
    let mut second = cloud::frames::FragmentAssembler::with_budget(budget);
    first
        .feed(StreamMsg::FrameBegin {
            len: (64 * 1024 * 1024) as u32,
        })
        .expect("the whole budget is available to one message");
    assert_eq!(
        second.feed(StreamMsg::FrameBegin { len: 1 }),
        Err(Error::Limit),
        "a second message must not exceed the aggregate budget"
    );
}

/// A failed fragment and a dropped assembler both release the session
/// reservation (RAII), so the next message can use the whole budget.
#[test]
fn assembler_error_and_drop_release_the_session_budget() {
    let max = 64 * 1024 * 1024;
    let budget = cloud::frames::ReassemblyBudget::new(max);

    let mut failed = cloud::frames::FragmentAssembler::with_budget(budget.clone());
    failed
        .feed(StreamMsg::FrameBegin { len: max as u32 })
        .expect("reserve everything");
    assert_eq!(
        failed.feed(StreamMsg::FrameEnd),
        Err(Error::Protocol),
        "underflow fails and must release"
    );

    let mut dropped = cloud::frames::FragmentAssembler::with_budget(budget.clone());
    dropped
        .feed(StreamMsg::FrameBegin { len: max as u32 })
        .expect("budget was released by the failed assembler");
    drop(dropped);

    let mut next = cloud::frames::FragmentAssembler::with_budget(budget);
    assert!(
        next.feed(StreamMsg::FrameBegin { len: max as u32 }).is_ok(),
        "drop must release the reservation"
    );
}

// ── §5: stream multiplexing rules for the new variants ──────────────────────

/// Chunk is allowed on an Attach stream inside the v2 grammar
/// (after FrameBegin, before FrameEnd). Chunk on non-Attach is the existing
/// upload body and is covered by the S4 tests; the v2-only records are
/// rejected there (see the forbidden_non_attach cases below).
#[test]
fn chunk_allowed_on_attach_only_inside_new_grammar() {
    let mut mux = cloud::frames::Mux::new(cloud::frames::Role::Initiator);
    let open = Inner {
        stream: 1,
        msg: StreamMsg::Open(cloud::frames::Op::Attach),
    };
    mux.feed(&open).expect("open");
    let begin = Inner {
        stream: 1,
        msg: StreamMsg::FrameBegin { len: 1 },
    };
    assert!(mux.feed(&begin).is_ok(), "Attach FrameBegin is legal");
    let chunk = Inner {
        stream: 1,
        msg: StreamMsg::Chunk(vec![1]),
    };
    assert!(mux.feed(&chunk).is_ok(), "Attach fragment Chunk is legal");
    let end = Inner {
        stream: 1,
        msg: StreamMsg::FrameEnd,
    };
    assert!(mux.feed(&end).is_ok(), "Attach FrameEnd is legal");
}

/// Head is still forbidden on an Attach stream.
#[test]
fn head_forbidden_on_attach() {
    let mut mux = cloud::frames::Mux::new(cloud::frames::Role::Initiator);
    let open = Inner {
        stream: 1,
        msg: StreamMsg::Open(cloud::frames::Op::Attach),
    };
    mux.feed(&open).expect("open");
    let head = Inner {
        stream: 1,
        msg: StreamMsg::Head {
            status: 200,
            content_type: "".into(),
            length: None,
        },
    };
    assert_eq!(mux.feed(&head), Err(Error::Protocol));
}

/// FrameBegin is forbidden on non-Attach streams.
#[test]
fn frame_begin_forbidden_non_attach() {
    let mut mux = cloud::frames::Mux::new(cloud::frames::Role::Initiator);
    let open = Inner {
        stream: 1,
        msg: StreamMsg::Open(cloud::frames::Op::List),
    };
    mux.feed(&open).expect("open");
    let begin = Inner {
        stream: 1,
        msg: StreamMsg::FrameBegin { len: 100 },
    };
    assert_eq!(mux.feed(&begin), Err(Error::Protocol));
}

/// FrameEnd is forbidden on non-Attach streams.
#[test]
fn frame_end_forbidden_non_attach() {
    let mut mux = cloud::frames::Mux::new(cloud::frames::Role::Initiator);
    let open = Inner {
        stream: 1,
        msg: StreamMsg::Open(cloud::frames::Op::List),
    };
    mux.feed(&open).expect("open");
    let end = Inner {
        stream: 1,
        msg: StreamMsg::FrameEnd,
    };
    assert_eq!(mux.feed(&end), Err(Error::Protocol));
}

/// Credit is accepted on a non-Attach stream.
#[test]
fn credit_is_accepted_on_non_attach_stream() {
    let mut mux = cloud::frames::Mux::new(cloud::frames::Role::Initiator);
    let open = Inner {
        stream: 1,
        msg: StreamMsg::Open(cloud::frames::Op::List),
    };
    mux.feed(&open).expect("open");
    let credit = Inner {
        stream: 1,
        msg: StreamMsg::Credit { bytes: 1024 },
    };
    assert!(
        mux.feed(&credit).is_ok(),
        "a Credit record on a non-Attach stream must be accepted"
    );
}

// ── §6: wire encoding ───────────────────────────────────────────────────────

/// FrameBegin serializes and deserializes.
#[test]
fn wire_roundtrip_frame_begin() {
    let inner = Inner {
        stream: 1,
        msg: StreamMsg::FrameBegin { len: 42 },
    };
    let bytes = cloud::frames::encode_inner(&inner);
    let decoded = cloud::frames::decode_inner(&bytes).expect("decode");
    assert_eq!(decoded, inner);
}

/// FrameEnd serializes and deserializes.
#[test]
fn wire_roundtrip_frame_end() {
    let inner = Inner {
        stream: 1,
        msg: StreamMsg::FrameEnd,
    };
    let bytes = cloud::frames::encode_inner(&inner);
    let decoded = cloud::frames::decode_inner(&bytes).expect("decode");
    assert_eq!(decoded, inner);
}

/// Credit serializes and deserializes.
#[test]
fn wire_roundtrip_credit() {
    let inner = Inner {
        stream: 1,
        msg: StreamMsg::Credit { bytes: 1024 },
    };
    let bytes = cloud::frames::encode_inner(&inner);
    let decoded = cloud::frames::decode_inner(&bytes).expect("decode");
    assert_eq!(decoded, inner);
}

// ── §7: version constants ───────────────────────────────────────────────────

/// PIPE_VERSION must be 3 for the symmetric-session pipe (pipe v3).
#[test]
fn pipe_version_is_3() {
    assert_eq!(cloud::constants::PIPE_VERSION, 3);
}
