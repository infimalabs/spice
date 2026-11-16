//! Reply, refusal, and packed crossing identity samples.
//!
//! This path module keeps the expanding response corpus separate from request-side samples.

use super::*;

/// The paged frames, sampled with and without an overflow tail.
pub(super) fn cover_packed_frames() {
    let rows = vec![Row {
        task: handle("SPICE-1kKRRsDPx"),
    }];
    let plain = Packed {
        rows: rows.clone(),
        overflow: None,
        horizon: seq(48213),
    };
    let overflowing = Packed {
        rows,
        overflow: Some(Overflow {
            omitted: 7,
            continuation: seq(48220),
        }),
        horizon: seq(48213),
    };

    law("Packed", &[plain.clone(), overflowing.clone()]);
    law(
        "Derived",
        &[
            Derived {
                fold: "board::ready".parse().expect("fold name"),
                r#gen: 12,
                cursor: seq(48213),
                packed: plain,
            },
            Derived {
                fold: "board::ready".parse().expect("fold name"),
                r#gen: 13,
                cursor: seq(48214),
                packed: overflowing,
            },
        ],
    );
}

/// The frames that carry a refusal or a fault back to a caller.
pub(super) fn cover_refusal_frames() {
    law(
        "SeqRange",
        &[
            SeqRange {
                first: seq(1),
                last: seq(2),
            },
            SeqRange {
                first: seq(1),
                last: seq(3),
            },
        ],
    );
    law(
        "RefusalDiagnostic",
        &[
            wire::RefusalDiagnostic::new("wire::mismatch", None, None, None),
            wire::RefusalDiagnostic::new(
                "witness::stale",
                Some("task".to_owned()),
                Some("48190".to_owned()),
                Some("48213".to_owned()),
            )
            .with_detail("read the current task"),
        ],
    );
    law(
        "SpiceCommand",
        &[
            wire::SpiceCommand::new("fold", std::iter::empty::<&str>())
                .expect("fixture command is valid"),
            wire::SpiceCommand::new("task", ["show", "CLAIM-1kFixture"])
                .expect("fixture command is valid"),
        ],
    );
    law(
        "Refusal",
        &[
            Refusal::repairable(
                wire::SpiceCommand::new("task", ["show", "CLAIM-1kFixture"])
                    .expect("fixture command is valid"),
                wire::RefusalDiagnostic::new("claim::not-held", None, None, None),
            ),
            stale_witness(),
            Refusal::invocation(
                "gate::prove-action-unregistered",
                "spice gate prove ACTION requires one registered gate action",
            ),
            Refusal::evidence(
                "gate::regressed",
                "file-shape: regressed",
                "repair the reported regression",
            ),
        ],
    );
    law(
        "Fault",
        &[
            moved_epoch(),
            Fault {
                class: FaultClass::Store,
                reason: "journal::unreadable".to_owned(),
                detail: "journal read failed".to_owned(),
            },
        ],
    );
}

/// The frames a caller reads back after a command lands.
pub(super) fn cover_reply_frames() {
    law(
        "DemoAnswer",
        &[
            DemoAnswer {
                root: "/tmp/spice-demo-one".to_owned(),
                transcript: "/tmp/spice-demo-one/spice-demo-transcript.jsonl".to_owned(),
                mode: "seed".to_owned(),
                lifecycle: "preserved".to_owned(),
                endpoint: None,
                launch_command: "spice demo --reuse /tmp/spice-demo-one".to_owned(),
                remove_command: "rm -rf -- /tmp/spice-demo-one".to_owned(),
            },
            DemoAnswer {
                root: "/tmp/spice-demo-two".to_owned(),
                transcript: "/tmp/spice-demo-two/spice-demo-transcript.jsonl".to_owned(),
                mode: "app".to_owned(),
                lifecycle: "removed".to_owned(),
                endpoint: Some("http://127.0.0.1:48213".to_owned()),
                launch_command: "spice demo --reuse /tmp/spice-demo-two".to_owned(),
                remove_command: "rm -rf -- /tmp/spice-demo-two".to_owned(),
            },
        ],
    );
    cover_journal_reply_frames();
    cover_restore_reply_frames();
    cover_transport_reply_frames();
}

fn cover_journal_reply_frames() {
    law(
        "JournalReport",
        &[
            JournalReport {
                schema: 1,
                cursor: None,
                facts: 0,
                entries: wire::output::Bounded {
                    rows: Vec::new(),
                    omission: None,
                },
            },
            JournalReport {
                schema: 1,
                cursor: Some(seq(19)),
                facts: 19,
                entries: wire::output::Bounded {
                    rows: Vec::new(),
                    omission: Some(Box::new(wire::output::Omission {
                        population: 19,
                        shown: 0,
                        withheld: 19,
                        shown_bytes: 0,
                        exhausted: wire::output::Exhausted::Rows,
                        next_bytes: 180,
                        limits: wire::output::Limits {
                            rows: 0,
                            bytes: 8_192,
                            text: 240,
                        },
                        continuation: wire::SpiceCommand::new("journal", ["--after", "19"])
                            .expect("fixture command is valid"),
                    })),
                },
            },
        ],
    );
}

fn cover_restore_reply_frames() {
    law(
        "JournalRestore",
        &[
            JournalRestore {
                plan: digest("1"),
                destination: "/repo/.git/.spice/data/spicejournal.sqlite3".to_owned(),
                previous: None,
                verification: restore_verification("2", 0),
                applied: false,
            },
            JournalRestore {
                plan: digest("3"),
                destination: "/repo/.git/.spice/data/spicejournal.sqlite3".to_owned(),
                previous: Some(digest("4")),
                verification: restore_verification("5", 19),
                applied: true,
            },
        ],
    );
}

fn cover_transport_reply_frames() {
    cover_release_note_frames();
    cover_proof_evidence_frames();
    law(
        "ReplyPosition",
        &[
            committed(48213),
            ReplyPosition::Snapshot(seq(48213)),
            ReplyPosition::Direct {},
        ],
    );
    law(
        "Reply",
        &[
            Reply {
                position: committed(48213),
                body: body("checked"),
            },
            Reply {
                position: ReplyPosition::Snapshot(seq(48213)),
                body: body("checked"),
            },
            Reply {
                position: ReplyPosition::Direct {},
                body: body("checked"),
            },
        ],
    );
    law(
        "Outcome",
        &[
            Outcome::Reply(Reply {
                position: committed(48213),
                body: body("checked"),
            }),
            Outcome::Reply(Reply {
                position: ReplyPosition::Direct {},
                body: body("checked directly"),
            }),
            Outcome::Refusal(stale_witness()),
        ],
    );
    law(
        "Response",
        &[
            Response {
                v: WireVersion(1),
                id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"),
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                outcome: Outcome::Reply(Reply {
                    position: committed(48213),
                    body: body("checked"),
                }),
            },
            Response {
                v: WireVersion(1),
                id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c4"),
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                outcome: Outcome::Reply(Reply {
                    position: ReplyPosition::Direct {},
                    body: body("checked directly"),
                }),
            },
            Response {
                v: WireVersion(1),
                id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
                outcome: Outcome::Fault(moved_epoch()),
            },
        ],
    );
}

fn cover_proof_evidence_frames() {
    law(
        "ProofEvidence",
        &[
            ProofEvidence {
                target: ReleaseTarget::Aarch64AppleDarwin,
                payload: digest("1"),
                receipt: digest("2"),
                battery: digest("3"),
                replay: digest("4"),
            },
            ProofEvidence {
                target: ReleaseTarget::X86_64UnknownLinuxMusl,
                payload: digest("5"),
                receipt: digest("6"),
                battery: digest("7"),
                replay: digest("8"),
            },
        ],
    );
}

fn cover_release_note_frames() {
    let entries = [
        ReleaseNotesEntry {
            topic: "/general".parse().expect("fixture topic"),
            subject: "Seed the repository".to_owned(),
            task: None,
            references: vec!["1111111111111111111111111111111111111111".to_owned()],
        },
        ReleaseNotesEntry {
            topic: "/dist/notes".parse().expect("fixture topic"),
            subject: "Derive release notes from history".to_owned(),
            task: Some("notes".to_owned()),
            references: vec![
                "2222222222222222222222222222222222222222".to_owned(),
                "3333333333333333333333333333333333333333".to_owned(),
            ],
        },
    ];
    law("ReleaseNotesEntry", &entries);
    law(
        "ReleaseNotes",
        &[
            ReleaseNotes {
                version: 1,
                identity: digest("1"),
                release_version: "unreleased".to_owned(),
                release_commit: "1111111111111111111111111111111111111111".to_owned(),
                previous_tag: None,
                bounded_recent_window: true,
                entries: vec![entries[0].clone()],
                document: "## Highlights\n".to_owned(),
            },
            ReleaseNotes {
                version: 1,
                identity: digest("2"),
                release_version: "1.2.3".to_owned(),
                release_commit: "3333333333333333333333333333333333333333".to_owned(),
                previous_tag: Some("v1.2.2".to_owned()),
                bounded_recent_window: false,
                entries: entries.to_vec(),
                document: "## Highlights\n\n- History-derived notes.\n".to_owned(),
            },
        ],
    );
}

fn restore_verification(nibble: &str, facts: u64) -> RestoreVerification {
    RestoreVerification {
        bundle: digest(nibble),
        wire: WireVersion::CURRENT,
        journal_schema: 1,
        authority: digest(nibble),
        members: 1,
        facts,
        receipts: 0,
        heads: 0,
        threads: facts,
    }
}
