//! RED tests: data frames and stream multiplexing (spec §9) — counter
//! gap/duplicate, AEAD failure, the 2^32 and 24 h limits (time injected),
//! mux encode/decode roundtrip, limits, Reset.
mod common;

use cloud::Error;
use cloud::frames::{
    self, FrameReceiver, FrameSender, Inner, Mux, MuxEvent, Op, RecvOutcome, Role, StreamMsg,
    ctr_allowed, decode_inner, encode_inner,
};
use cloud::handshake::Data;

const SID: [u8; 16] = [0x09; 16];
const K_IR: [u8; 32] = [0x01; 32];

fn inner_attach_open() -> Inner {
    Inner {
        stream: 1,
        msg: StreamMsg::Open(Op::Attach),
    }
}

/// Seal one inner through the sender and return the Data frame.
fn seal(sender: &mut FrameSender, inner: &Inner) -> Data {
    sender.seal(&encode_inner(inner)).expect("seal")
}

// -- record protection roundtrip ------------------------------------------------------

#[test]
fn roundtrip_seal_open() {
    let mut s = FrameSender::new(SID, K_IR);
    let mut r = FrameReceiver::new(SID, K_IR);
    let d = seal(&mut s, &inner_attach_open());
    let out = r.open(&d).expect("open");
    assert_eq!(out, RecvOutcome::Inner(inner_attach_open()));
    assert!(!r.closed());
}

// -- strict receiver (spec §9.2: any failure closes the session) ------------------------

#[test]
fn counter_gap_closes_session() {
    let mut s = FrameSender::new(SID, K_IR);
    let mut r = FrameReceiver::new(SID, K_IR);
    let d0 = seal(&mut s, &inner_attach_open());
    let _d1 = seal(&mut s, &inner_attach_open());
    let d2 = seal(&mut s, &inner_attach_open());
    let _ = r.open(&d0).expect("first ok");
    // Skip d1, deliver d2 → gap.
    assert_eq!(r.open(&d2), Err(Error::Sequence));
    assert!(r.closed(), "gap must close the session");
}

#[test]
fn duplicate_ctr_closes_session() {
    let mut s = FrameSender::new(SID, K_IR);
    let mut r = FrameReceiver::new(SID, K_IR);
    let d0 = seal(&mut s, &inner_attach_open());
    let _ = r.open(&d0).expect("first ok");
    assert_eq!(
        r.open(&d0),
        Err(Error::Sequence),
        "duplicate must be Sequence"
    );
    assert!(r.closed());
}

#[test]
fn aead_tag_flip_closes_session() {
    let mut s = FrameSender::new(SID, K_IR);
    let mut r = FrameReceiver::new(SID, K_IR);
    let mut d = seal(&mut s, &inner_attach_open());
    d.ct[0] ^= 1;
    assert_eq!(r.open(&d), Err(Error::DecryptionError));
    assert!(r.closed());
}

#[test]
fn wrong_sid_dropped_silently_session_intact() {
    let other: [u8; 16] = [0x0A; 16];
    let mut s = FrameSender::new(other, K_IR);
    let mut r = FrameReceiver::new(SID, K_IR);
    let d = seal(&mut s, &inner_attach_open());
    assert_eq!(r.open(&d), Ok(RecvOutcome::Dropped));
    assert!(!r.closed(), "a stale-sid frame must not close the session");
    // The session still works.
    let mut s2 = FrameSender::new(SID, K_IR);
    let d2 = seal(&mut s2, &inner_attach_open());
    assert_eq!(r.open(&d2), Ok(RecvOutcome::Inner(inner_attach_open())));
}

// -- limits (spec §9.6): MAX_RECORDS = 2^32, SESSION_LIFETIME = 24 h ----------------------

#[test]
fn ctr_at_2_32_not_allowed() {
    assert!(ctr_allowed(0));
    assert!(ctr_allowed(u64::from(u32::MAX)));
    assert!(
        !ctr_allowed(u64::from(u32::MAX) + 1),
        "ctr = 2^32 must be refused"
    );
}

#[test]
fn receiver_aborts_on_ctr_ge_2_32() {
    // The range check precedes the equality check (spec §9.6).
    let mut r = FrameReceiver::new(SID, K_IR);
    let d = Data {
        sid: SID,
        ctr: u64::from(u32::MAX) + 1,
        ct: vec![0u8; 16],
    };
    assert_eq!(r.open(&d), Err(Error::Limit));
    assert!(r.closed());
}

#[test]
fn session_expires_at_24h() {
    let t = 1_800_000_000u64;
    assert!(!frames::session_expired(t, t + 86_399));
    assert!(
        frames::session_expired(t, t + 86_400),
        "24 h boundary is inclusive"
    );
}

#[test]
fn session_expired_checked_arithmetic_no_panic() {
    // Underflow (now < established_at) is "out of range", never a panic
    // (spec §1.5).
    let res = std::panic::catch_unwind(|| frames::session_expired(u64::MAX, 0));
    assert!(res.is_ok(), "session_expired must not panic on underflow");
    let res = std::panic::catch_unwind(|| frames::session_expired(0, u64::MAX));
    assert!(res.is_ok());
    assert!(frames::session_expired(0, u64::MAX));
}

// -- inner bincode (spec §9.4): every StreamMsg and Op variant ----------------------------

#[test]
fn inner_roundtrip_all_stream_msg_variants() {
    let cases: Vec<Inner> = vec![
        Inner {
            stream: 1,
            msg: StreamMsg::Open(Op::Attach),
        },
        Inner {
            stream: 1,
            msg: StreamMsg::Frame("{\"type\":\"attach\"}".into()),
        },
        Inner {
            stream: 2,
            msg: StreamMsg::Chunk(vec![1, 2, 3]),
        },
        Inner {
            stream: 3,
            msg: StreamMsg::Head {
                status: 200,
                content_type: "text/plain".into(),
                length: Some(42),
            },
        },
        Inner {
            stream: 3,
            msg: StreamMsg::Head {
                status: 500,
                content_type: "".into(),
                length: None,
            },
        },
        Inner {
            stream: 4,
            msg: StreamMsg::End,
        },
        Inner {
            stream: 4,
            msg: StreamMsg::Reset(3),
        },
    ];
    for c in cases {
        let bytes = encode_inner(&c);
        assert_eq!(decode_inner(&bytes).expect("decode"), c);
    }
}

#[test]
fn inner_roundtrip_all_op_variants() {
    let ops: Vec<Op> = vec![
        Op::Attach,
        Op::Profiles,
        Op::List,
        Op::Kill { id: [0xAB; 16] },
        Op::Get(cloud::frames::GetTarget::Image { id: "img1".into() }),
        Op::Get(cloud::frames::GetTarget::Upload {
            convo: [0xCD; 16],
            id: "up1".into(),
        }),
        Op::Upload {
            convo: [0xEF; 16],
            name: "a.txt".into(),
            content_type: "text/plain".into(),
        },
    ];
    for op in ops {
        let c = Inner {
            stream: 5,
            msg: StreamMsg::Open(op.clone()),
        };
        assert_eq!(decode_inner(&encode_inner(&c)).expect("decode"), c);
    }
}

#[test]
fn decode_inner_over_max_inner_rejected() {
    // A Chunk over MAX_CHUNK makes the bincode encoding exceed the limit
    // when pushed past MAX_INNER.
    let big = Inner {
        stream: 1,
        msg: StreamMsg::Chunk(vec![0u8; cloud::constants::MAX_INNER + 1]),
    };
    assert_eq!(decode_inner(&encode_inner(&big)), Err(Error::Protocol));
}

// -- stream rules (spec §9.5) ------------------------------------------------------------------

#[test]
fn mux_open_from_responder_aborts() {
    let mut r = Mux::new(Role::Responder);
    assert_eq!(
        r.feed(&Inner {
            stream: 1,
            msg: StreamMsg::Open(Op::Attach)
        }),
        Err(Error::Protocol)
    );
}

#[test]
fn mux_stream_id_zero_reserved() {
    let mut m = Mux::new(Role::Responder);
    assert_eq!(
        m.feed(&Inner {
            stream: 0,
            msg: StreamMsg::Open(Op::Attach)
        }),
        Err(Error::Protocol)
    );
}

#[test]
fn mux_non_increasing_stream_id_aborts() {
    let mut m = Mux::new(Role::Responder);
    assert!(
        m.feed(&Inner {
            stream: 2,
            msg: StreamMsg::Open(Op::Attach)
        })
        .is_ok()
    );
    assert_eq!(
        m.feed(&Inner {
            stream: 2,
            msg: StreamMsg::Open(Op::Attach)
        }),
        Err(Error::Protocol),
        "re-using an id is non-increasing"
    );
    assert_eq!(
        m.feed(&Inner {
            stream: 1,
            msg: StreamMsg::Open(Op::Attach)
        }),
        Err(Error::Protocol),
        "a smaller id is non-increasing"
    );
}

#[test]
fn mux_chunk_on_attach_aborts() {
    let mut m = Mux::new(Role::Responder);
    assert!(
        m.feed(&Inner {
            stream: 1,
            msg: StreamMsg::Open(Op::Attach)
        })
        .is_ok()
    );
    assert_eq!(
        m.feed(&Inner {
            stream: 1,
            msg: StreamMsg::Chunk(vec![1])
        }),
        Err(Error::Protocol)
    );
}

#[test]
fn mux_head_on_attach_aborts() {
    let mut m = Mux::new(Role::Responder);
    assert!(
        m.feed(&Inner {
            stream: 1,
            msg: StreamMsg::Open(Op::Attach)
        })
        .is_ok()
    );
    assert_eq!(
        m.feed(&Inner {
            stream: 1,
            msg: StreamMsg::Head {
                status: 200,
                content_type: "".into(),
                length: None
            }
        }),
        Err(Error::Protocol)
    );
}

#[test]
fn mux_frame_on_non_attach_aborts() {
    let mut m = Mux::new(Role::Responder);
    assert!(
        m.feed(&Inner {
            stream: 1,
            msg: StreamMsg::Open(Op::Profiles)
        })
        .is_ok()
    );
    assert_eq!(
        m.feed(&Inner {
            stream: 1,
            msg: StreamMsg::Frame("{}".into())
        }),
        Err(Error::Protocol)
    );
}

#[test]
fn mux_chunk_over_max_chunk_aborts() {
    let mut m = Mux::new(Role::Responder);
    assert!(
        m.feed(&Inner {
            stream: 1,
            msg: StreamMsg::Open(Op::List)
        })
        .is_ok()
    );
    assert_eq!(
        m.feed(&Inner {
            stream: 1,
            msg: StreamMsg::Chunk(vec![0u8; cloud::constants::MAX_CHUNK + 1])
        }),
        Err(Error::Protocol)
    );
}

#[test]
fn mux_message_for_never_opened_id_aborts() {
    let mut m = Mux::new(Role::Responder);
    assert_eq!(
        m.feed(&Inner {
            stream: 7,
            msg: StreamMsg::Frame("{}".into())
        }),
        Err(Error::Protocol)
    );
}

#[test]
fn mux_reset_ends_stream_both_directions() {
    let mut m = Mux::new(Role::Responder);
    assert!(
        m.feed(&Inner {
            stream: 1,
            msg: StreamMsg::Open(Op::Attach)
        })
        .is_ok()
    );
    let events = m
        .feed(&Inner {
            stream: 1,
            msg: StreamMsg::Reset(5),
        })
        .expect("reset ok");
    assert!(events.contains(&MuxEvent::Reset(1, 5)));
    // S5: in-flight messages for a reset id are dropped silently.
    let events = m
        .feed(&Inner {
            stream: 1,
            msg: StreamMsg::Frame("{}".into()),
        })
        .expect("dropped");
    assert!(
        events.is_empty(),
        "post-reset message must be dropped silently"
    );
}

#[test]
fn mux_end_both_sides_closes_stream() {
    // Attach: each side ends its direction; the stream closes when both
    // have sent End (S3). The responder side sees the initiator's End
    // (event) and its own End (event) → Closed after the second.
    let mut r = Mux::new(Role::Responder);
    let mut i = Mux::new(Role::Initiator);
    let open = Inner {
        stream: 1,
        msg: StreamMsg::Open(Op::Attach),
    };
    assert!(i.feed(&open).is_ok());
    assert!(r.feed(&open).is_ok());
    let end = Inner {
        stream: 1,
        msg: StreamMsg::End,
    };
    assert!(i.feed(&end).is_ok());
    assert!(r.feed(&end).is_ok(), "first End does not close");
    // The peer's End arrives: both directions ended → Closed.
    let events = r.feed(&end).expect("peer end");
    assert!(events.contains(&MuxEvent::Closed(1)));
}

#[test]
fn mux_max_64_streams_answers_reset_too_large() {
    let mut m = Mux::new(Role::Responder);
    for id in 1..=64u32 {
        assert!(
            m.feed(&Inner {
                stream: id,
                msg: StreamMsg::Open(Op::Attach)
            })
            .is_ok(),
            "open {id} must be accepted"
        );
    }
    let events = m
        .feed(&Inner {
            stream: 65,
            msg: StreamMsg::Open(Op::Attach),
        })
        .expect("S2 is an answer, not an abort");
    assert!(
        events.contains(&MuxEvent::AnswerReset(
            65,
            cloud::constants::reset::TOO_LARGE
        )),
        "65th open must answer Reset(TooLarge), got {events:?}"
    );
}

#[test]
fn normalize_reset_unknown_is_internal() {
    assert_eq!(frames::normalize_reset(1), 1);
    assert_eq!(frames::normalize_reset(6), 6);
    assert_eq!(
        frames::normalize_reset(0),
        cloud::constants::reset::INTERNAL
    );
    assert_eq!(
        frames::normalize_reset(7),
        cloud::constants::reset::INTERNAL
    );
}
