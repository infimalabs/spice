//! The word an exiting occupant's transcript ends on.
//!
//! An occupant whose prompt the vendor refused can exit cleanly, so its exit
//! status says nothing and the notice for the refusal may still be unread when
//! the exit is accounted. The tape is durable before either notice is sent.
//! A typed prompt result, once the session is settled, is the sole authority
//! for the turn; records without that current contract leave the exit
//! undecided.

use super::*;

/// Most tail bytes read back looking for the occupant's newest event.
///
/// The final records of a refused turn are small; a tail this long is read
/// only when they sit behind large tool results.
const TAIL_BYTES: u64 = 1024 * 1024;

/// What an exiting occupant's authoritative prompt result says about its exit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum LastWord {
    /// The vendor refused the turn, in these words.
    Failure(String),
    /// The vendor completed a settled prompt.
    Answered,
    /// Nothing decides the exit: the current prompt did not settle, no current
    /// prompt result exists, or the transcript could not be read.
    Silent,
}

/// The word the exiting occupant's transcript ends on.
///
/// Only records at or after the occupant's own session declaration are read,
/// so a failure an earlier occupant left on the tape never holds the lane
/// against a successor that wrote nothing. An occupant that declared no
/// session has no bounded range and is left to ordinary death accounting.
/// Every reading failure is written down and answers [`LastWord::Silent`]:
/// the exit is accounted either way, and this decides only how.
pub(super) fn exit_word(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<LastWord> {
    let observed = current_residency(bus, epoch)?;
    let Some(session) = observed
        .body()
        .lanes
        .iter()
        .find(|row| row.lane == lane && row.occupant == Some(occupant))
        .and_then(|row| row.session.as_ref())
        .filter(|session| session.occupant == occupant)
    else {
        return Ok(LastWord::Silent);
    };
    let Some(vendor) = ingest_vendor(&session.vendor) else {
        return Ok(LastWord::Silent);
    };
    let root = paths.lane_tape(lane).join(session.vendor.as_str());
    match closing_word(&root, vendor, &session.locus) {
        Ok(word) => Ok(word),
        Err(error) => {
            let _ = writeln!(
                diagnostic,
                "lane {lane} occupant {occupant} closing transcript unread; exit accounted without it: {error}"
            );
            Ok(LastWord::Silent)
        }
    }
}

/// The word the newest authoritative prompt result at or after `declared`
/// carries, read with the vendor's dialect.
pub(super) fn closing_word(
    root: &Path,
    vendor: &spice::extension::Vendor,
    declared: &wire::TapeLocus,
) -> Result<LastWord, String> {
    let tape = tape::Tape::open_existing(root).map_err(|error| error.to_string())?;
    let extents = tape.extents().map_err(|error| error.to_string())?;
    let mut remaining = TAIL_BYTES;
    let mut settled = false;
    for extent in extents.into_iter().rev() {
        if extent.member < declared.member || remaining == 0 {
            break;
        }
        let start = if extent.member == declared.member {
            declared.offset
        } else {
            0
        };
        let end = extent.length;
        let length = end.saturating_sub(start).min(remaining);
        if length == 0 {
            continue;
        }
        let offset = end - length;
        let bytes = tape
            .read(tape::Locus::new(extent.member, offset, length))
            .map_err(|error| error.to_string())?;
        remaining -= length;
        // A window cut short of the declaration may open mid-record; only a
        // range that follows a newline, or starts at the declaration, is whole.
        let whole_from_start = offset == start;
        for (from, to) in record_ranges(&bytes, whole_from_start).into_iter().rev() {
            let locus = tape::Locus::new(
                extent.member,
                offset + u64::try_from(from).map_err(|error| error.to_string())?,
                u64::try_from(to - from).map_err(|error| error.to_string())?,
            );
            let decoded = vendor
                .decode(&bytes[from..to], locus)
                .map_err(|error| error.to_string())?;
            let Some(context) = decoded.context() else {
                continue;
            };
            settled |= context.session_settled;
            let Some(outcome) = context
                .prompt_outcome
                .as_ref()
                .filter(|outcome| outcome.agent_invoked)
            else {
                if context.turn == Some(listen::TurnMoment::Started) {
                    return Ok(LastWord::Silent);
                }
                continue;
            };
            return Ok(match outcome.status {
                listen::PromptStatus::Completed if outcome.session_settled || settled => {
                    LastWord::Answered
                }
                listen::PromptStatus::Error => decoded
                    .events()
                    .iter()
                    .find_map(|event| match event {
                        listen::Event::Failure(failure) => {
                            Some(LastWord::Failure(failure.message.clone()))
                        }
                        _ => None,
                    })
                    .unwrap_or(LastWord::Silent),
                listen::PromptStatus::Completed | listen::PromptStatus::Aborted => LastWord::Silent,
            });
        }
    }
    Ok(LastWord::Silent)
}

/// Complete newline-terminated record ranges in `bytes`, skipping a leading
/// partial record unless the window is known to open on a record boundary.
fn record_ranges(bytes: &[u8], whole_from_start: bool) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = if whole_from_start { Some(0) } else { None };
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        if let Some(from) = start {
            ranges.push((from, index + 1));
        }
        start = Some(index + 1);
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const REFUSAL: &str =
        "Refusal (reasoning_extraction): This request was blocked by our content filtering policy.";

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "spice-last-word-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).expect("last-word scratch root is unique");
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("last-word scratch is removed");
        }
    }

    fn write(tape: &tape::Tape, record: &str) -> wire::TapeLocus {
        let locus = tape
            .tee(format!("{record}\n").as_bytes())
            .expect("fixture record appends");
        wire::TapeLocus {
            member: locus.member,
            offset: locus.offset,
            length: locus.length,
        }
    }

    fn omp_declaration(session: &str) -> String {
        format!(
            r#"{{"id":"spice-state","type":"response","command":"get_state","success":true,"data":{{"sessionId":"{session}"}}}}"#
        )
    }

    fn omp_prompt_result(status: &str, settled: bool, error: Option<&str>) -> String {
        let error = error.map_or(String::new(), |message| {
            format!(
                r#","error":{{"message":"{message}","provider":"openai","model":"gpt-5.4","httpStatus":429,"retryable":true}}"#
            )
        });
        format!(
            r#"{{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"{status}","sessionSettled":{settled}{error}}}"#
        )
    }

    const OMP_SESSION_SETTLED: &str = r#"{"type":"session_settled","reason":"prompt_complete"}"#;

    const OMP_USER: &str = r#"{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"continue"}]}}"#;
    const OMP_WIDGET: &str =
        r#"{"type":"extension_ui_request","id":"1583f94d24ebac0b","method":"setWidget"}"#;
    const OMP_AGENT_END: &str = r#"{"type":"agent_end","messages":[]}"#;

    fn refused_wake(tape: &tape::Tape, session: &str) -> wire::TapeLocus {
        write(tape, r#"{"type":"ready","protocolVersion":1}"#);
        let declared = write(tape, &omp_declaration(session));
        write(tape, &omp_prompt_result("error", true, Some(REFUSAL)));
        declared
    }

    #[test]
    fn a_turn_that_ended_on_the_operators_prompt_is_silent() {
        let scratch = Scratch::new();
        let tape = tape::Tape::open(&scratch.0).expect("fixture tape opens");
        let declared = write(
            &tape,
            &omp_declaration("018f82d4-6f59-7d06-92e3-bce9131163c2"),
        );
        write(&tape, r#"{"type":"agent_start"}"#);
        write(&tape, OMP_USER);
        write(&tape, OMP_WIDGET);

        let word = closing_word(&scratch.0, &spice::extension::OMP, &declared)
            .expect("the retained tape reads");
        assert_eq!(word, LastWord::Silent);
    }

    #[test]
    fn prompt_completion_requires_the_reported_session_to_settle() {
        let scratch = Scratch::new();
        let tape = tape::Tape::open(&scratch.0).expect("fixture tape opens");
        let declared = write(
            &tape,
            &omp_declaration("018f82d4-6f59-7d06-92e3-bce9131163c2"),
        );
        write(&tape, &omp_prompt_result("completed", false, None));

        assert_eq!(
            closing_word(&scratch.0, &spice::extension::OMP, &declared)
                .expect("the retained tape reads"),
            LastWord::Silent,
            "a completed prompt with live background work has not settled"
        );

        write(&tape, OMP_SESSION_SETTLED);
        assert_eq!(
            closing_word(&scratch.0, &spice::extension::OMP, &declared)
                .expect("the retained tape reads"),
            LastWord::Answered,
            "the later typed settlement completes the prompt"
        );
    }

    #[test]
    fn non_agent_asides_do_not_hide_the_settled_prompt() {
        let scratch = Scratch::new();
        let tape = tape::Tape::open(&scratch.0).expect("fixture tape opens");
        let declared = write(
            &tape,
            &omp_declaration("018f82d4-6f59-7d06-92e3-bce9131163c2"),
        );
        write(&tape, &omp_prompt_result("completed", true, None));
        write(
            &tape,
            r#"{"type":"prompt_result","id":"spice-aside-1","agentInvoked":false,"status":"completed","sessionSettled":false}"#,
        );

        assert_eq!(
            closing_word(&scratch.0, &spice::extension::OMP, &declared)
                .expect("the retained tape reads"),
            LastWord::Answered,
            "an aside does not replace the authoritative agent result"
        );
    }

    #[test]
    fn a_new_prompt_prevents_reusing_the_preceding_settled_result() {
        let scratch = Scratch::new();
        let tape = tape::Tape::open(&scratch.0).expect("fixture tape opens");
        let declared = write(
            &tape,
            &omp_declaration("018f82d4-6f59-7d06-92e3-bce9131163c2"),
        );
        write(&tape, &omp_prompt_result("completed", true, None));
        write(&tape, r#"{"type":"agent_start"}"#);

        assert_eq!(
            closing_word(&scratch.0, &spice::extension::OMP, &declared)
                .expect("the retained tape reads"),
            LastWord::Silent,
            "the newest prompt has no result of its own"
        );
    }

    #[test]
    fn prompt_result_provider_failure_is_the_exit_word() {
        let scratch = Scratch::new();
        let tape = tape::Tape::open(&scratch.0).expect("fixture tape opens");
        let declared = write(
            &tape,
            &omp_declaration("018f82d4-6f59-7d06-92e3-bce9131163c2"),
        );
        write(
            &tape,
            &omp_prompt_result("error", true, Some("capacity exhausted")),
        );

        assert_eq!(
            closing_word(&scratch.0, &spice::extension::OMP, &declared)
                .expect("the retained tape reads"),
            LastWord::Failure("capacity exhausted".to_owned())
        );
    }

    #[test]
    fn a_predecessor_failure_before_the_declaration_never_holds_the_successor() {
        let scratch = Scratch::new();
        let tape = tape::Tape::open(&scratch.0).expect("fixture tape opens");
        let predecessor = refused_wake(&tape, "018f82d4-6f59-7d06-92e3-bce9131163c2");
        assert_eq!(
            closing_word(&scratch.0, &spice::extension::OMP, &predecessor)
                .expect("the retained tape reads"),
            LastWord::Failure(REFUSAL.to_owned()),
            "the predecessor's exit is accounted on its refusal"
        );

        let successor = write(
            &tape,
            &omp_declaration("018f82d4-6f59-7d06-92e3-bce9131163c3"),
        );
        write(&tape, r#"{"type":"agent_start"}"#);
        write(&tape, OMP_AGENT_END);
        assert_eq!(
            closing_word(&scratch.0, &spice::extension::OMP, &successor)
                .expect("the retained tape reads"),
            LastWord::Silent,
            "the successor wrote no event after its declaration"
        );
        let beyond = wire::TapeLocus {
            member: successor.member,
            offset: successor.offset + 4096,
            length: 1,
        };
        assert_eq!(
            closing_word(&scratch.0, &spice::extension::OMP, &beyond)
                .expect("a declaration past the tape reads nothing"),
            LastWord::Silent
        );
    }

    #[test]
    fn the_scan_crosses_sealed_members_back_to_the_declaration() {
        let scratch = Scratch::new();
        let tape = tape::Tape::open(&scratch.0).expect("fixture tape opens");
        write(
            &tape,
            &omp_prompt_result("error", true, Some("an earlier occupant's refusal")),
        );
        let declared = write(
            &tape,
            &omp_declaration("018f82d4-6f59-7d06-92e3-bce9131163c2"),
        );
        tape.seal().expect("first member seals");
        write(&tape, OMP_USER);
        tape.seal().expect("second member seals");
        write(&tape, &omp_prompt_result("error", true, Some(REFUSAL)));
        assert_eq!(declared.member, 1);

        assert_eq!(
            closing_word(&scratch.0, &spice::extension::OMP, &declared)
                .expect("the sealed tape reads"),
            LastWord::Failure(REFUSAL.to_owned())
        );

        let later = wire::TapeLocus {
            member: 3,
            offset: 0,
            length: 1,
        };
        assert_eq!(
            closing_word(&scratch.0, &spice::extension::OMP, &later)
                .expect("the sealed tape reads"),
            LastWord::Failure(REFUSAL.to_owned())
        );
        let last_member_only = wire::TapeLocus {
            member: 2,
            offset: 0,
            length: 1,
        };
        assert_eq!(
            closing_word(&scratch.0, &spice::extension::OMP, &last_member_only)
                .expect("the sealed tape reads"),
            LastWord::Failure(REFUSAL.to_owned())
        );
    }

    #[test]
    fn a_missing_tape_is_a_reading_failure_not_a_verdict() {
        let scratch = Scratch::new();
        let declared = wire::TapeLocus {
            member: 1,
            offset: 0,
            length: 1,
        };
        closing_word(&scratch.0.join("absent"), &spice::extension::OMP, &declared)
            .expect_err("an absent tape cannot decide the exit");
    }

    #[test]
    fn record_ranges_skip_a_leading_partial_unless_the_window_opens_on_a_boundary() {
        let bytes = b"tail-of-record\nwhole\nlast\npartial";
        assert_eq!(record_ranges(bytes, false), [(15, 21), (21, 26)]);
        assert_eq!(record_ranges(bytes, true), [(0, 15), (15, 21), (21, 26)]);
        assert!(record_ranges(b"no newline", false).is_empty());
        assert!(record_ranges(b"", true).is_empty());
    }
}
