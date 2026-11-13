//! Durable global app settings and their command surface.
//!
//! The settings answer carries every born principal's display identity,
//! derived from that principal's recorded profile at the answer's horizon;
//! settings never carry or change it. Git configuration only seeds the profile
//! an admission records.

use bus::Bus;
use gix::bstr::ByteSlice as _;
use journal::FactCause;
use spice::repository::Repository;
use wire::intent::{CommandAnswer, GlobalSettingsAnswer, GlobalSettingsSet};
use wire::principal::{PrincipalDirectory, PrincipalProfile, PrincipalRoles};
use wire::{Outcome, Producer, Reply, ReplyPosition, SeqRange};

use super::shutdown::{current_residency, guard_residency};
use super::{
    CommandEffects, CommandResult, commit, seq, store_fault, typed_command, typed_outcome, write,
};

fn stored_fast_mode(envelope: Option<&wire::Envelope<wire::AnyFact>>) -> bool {
    envelope
        .and_then(|envelope| envelope.fact.read::<wire::body::GlobalSettingsChanged>())
        .is_some_and(|settings| settings.fast_mode)
}

pub(super) fn normalize(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(super) fn malformed(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let value = value.trim();
        !value.is_empty() && value.chars().any(char::is_control)
    })
}

/// The profile the repository's Git `user.name` and `user.email` describe,
/// which an admission copies when it is given none.
pub(super) fn git_profile(repository: &Repository) -> CommandResult<PrincipalProfile> {
    let git = gix::open(repository.worktree_root()).map_err(|error| {
        super::environment_fault("principal::git-profile-unavailable", &error.to_string())
    })?;
    let config = git.config_snapshot();
    let read = |key| {
        config
            .string(key)
            .and_then(|value| value.to_str().ok().and_then(|value| normalize(Some(value))))
            .filter(|value| !malformed(Some(value)))
    };
    Ok(PrincipalProfile {
        name: read("user.name"),
        email: read("user.email"),
    })
}

fn answer(fast_mode: bool, roles: &PrincipalRoles) -> GlobalSettingsAnswer {
    GlobalSettingsAnswer {
        fast_mode,
        principals: PrincipalDirectory::display(roles.heads()),
    }
}

/// Read the global fast-mode flag at one journal horizon.
pub(super) fn global_fast_mode(bus: &Bus) -> CommandResult<bool> {
    bus.read(|read| {
        Ok(stored_fast_mode(
            read.latest_fact(wire::FactKind::GlobalSettingsChanged)?
                .as_ref(),
        ))
    })
    .map_err(|error| store_fault("settings::journal-unreadable", &error.to_string()))
}

/// Resolve one seat's fast mode for its next occupant launch across every
/// level, off while its vendor's refusal of fast mode stands.
pub(super) fn lane_fast_mode(
    bus: &Bus,
    head: &wire::ResidencyHead,
    row: &wire::LaneResidency,
) -> CommandResult<bool> {
    Ok(head.fast_mode(row, global_fast_mode(bus)?) && !row.refuses_fast_mode())
}

/// Read the current global settings at one immutable journal horizon.
pub(super) fn read(bus: &Bus) -> Outcome<CommandAnswer> {
    let (cursor, fact, roles) = match bus.read(|read| {
        Ok((
            read.cursor()?,
            read.latest_fact(wire::FactKind::GlobalSettingsChanged)?,
            read.principal_roles()?,
        ))
    }) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return store_fault("settings::journal-unreadable", &error.to_string());
        }
    };
    let Some(cursor) = cursor else {
        return store_fault(
            "settings::journal-empty",
            "a running resident has no startup authority fact",
        );
    };
    let answer = answer(stored_fast_mode(fact.as_ref()), &roles);
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(cursor),
        body: CommandAnswer::GlobalSettings(answer),
    })
}

/// Commit one global settings replacement and its replayable command receipt,
/// then owe each running member whose effective fast mode it flips a live
/// change. Turning the setting on also forgets every seat's fast-mode
/// refusal in the same transaction, so each next launch tries fast mode.
pub(super) fn set(
    bus: &Bus,
    epoch: wire::CanonicalUuid,
    command: &wire::AdmittedCommand,
    request: &GlobalSettingsSet,
    effects: &mut CommandEffects,
) -> Outcome<CommandAnswer> {
    let answer = match super::principal_surface::roles(bus) {
        Ok(roles) => answer(request.fast_mode, &roles),
        Err(outcome) => return typed_outcome(outcome),
    };
    let (global, observed) = match global_fast_mode(bus)
        .and_then(|global| current_residency(bus, epoch).map(|observed| (global, observed)))
    {
        Ok(current) => current,
        Err(outcome) => return typed_outcome(outcome),
    };
    let forgotten = if answer.fast_mode && !global {
        observed.body().forget_fast_refusals(|_| true)
    } else {
        None
    };
    let mut proposal = match write(command.actor().clone(), epoch, Producer::CommandDoor) {
        Ok(proposal) => proposal,
        Err(outcome) => return typed_outcome(outcome),
    };
    let changed = match proposal.fact(
        wire::body::GlobalSettingsChanged::new(answer.fast_mode),
        FactCause::Root,
    ) {
        Ok(changed) => changed,
        Err(error) => return store_fault("settings::fact-invalid", &error.to_string()),
    };
    if let Some(next) = forgotten {
        guard_residency(&mut proposal, &observed);
        proposal.residency_head(changed, move |_| next);
    }
    if let Err(error) = proposal.command_result(
        typed_command(command, request.clone()),
        epoch,
        answer.clone(),
    ) {
        return store_fault("settings::receipt-invalid", &error.to_string());
    }
    match commit(bus, proposal) {
        Ok(committed) => {
            super::lane_surface::owe_global_fast_flips(
                effects,
                command,
                observed.body(),
                global,
                answer.fast_mode,
            );
            Outcome::Reply(Reply {
                position: ReplyPosition::Committed(SeqRange {
                    first: seq(committed.first()),
                    last: seq(committed.last()),
                }),
                body: CommandAnswer::GlobalSettings(answer),
            })
        }
        Err(outcome) => typed_outcome(outcome),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_take_the_first_two_words_of_the_recorded_name() {
        let one_word = PrincipalProfile {
            name: Some("c".to_owned()),
            email: None,
        };
        assert_eq!(one_word.initials().as_deref(), Some("C"));
        assert_eq!(
            one_word
                .display(uuid(1), "1".parse().expect("principal code"))
                .gravatar,
            None
        );

        let three_words = PrincipalProfile {
            name: Some("céline adams third".to_owned()),
            email: Some("git@example.invalid".to_owned()),
        };
        assert_eq!(three_words.initials().as_deref(), Some("CA"));
        assert!(
            three_words
                .display(uuid(2), "2".parse().expect("principal code"))
                .gravatar
                .is_some()
        );
    }

    #[test]
    fn identity_normalization_clears_blank_values_and_refuses_controls() {
        assert_eq!(normalize(Some("  C A  ")).as_deref(), Some("C A"));
        assert_eq!(normalize(Some(" \t ")), None);
        assert!(malformed(Some("C\nA")));
        assert!(!malformed(Some("C A")));
    }

    fn uuid(last: u8) -> wire::CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131164{last:02}")
            .parse()
            .expect("fixture UUID")
    }

    /// Seat two lanes whose vendor refused fast mode for the model they run.
    fn seat_refusals(bus: &Bus, epoch: wire::CanonicalUuid) {
        let lanes = [1, 2]
            .map(|index| {
                let (lane, occupant) = (uuid(index), uuid(index + 10));
                let mut launch = wire::body::LaneLaunchIdentity::new("mockgw/gpt-5.4", "high");
                launch.fast_refusal = Some("priority refused".to_owned());
                wire::LaneResidency {
                    lane,
                    team: lane,
                    name: None,
                    worktree: "/fleet".to_owned(),
                    life: "drive".to_owned(),
                    vendor: "omp".parse().expect("fixture vendor"),
                    model: None,
                    effort: None,
                    fast_mode: false,
                    occupant: Some(occupant),
                    lineage: vec![occupant],
                    capabilities: vec![uuid(index + 20)],
                    session: Some(wire::LaneSession {
                        lane,
                        occupant,
                        worktree: "/fleet".to_owned(),
                        vendor: "omp".parse().expect("fixture vendor"),
                        launch,
                        epoch,
                        stream: "stdout".to_owned(),
                        identity: format!("session-{index}"),
                        locus: wire::TapeLocus {
                            member: 1,
                            offset: 0,
                            length: 1,
                        },
                    }),
                    disposition: "occupied".to_owned(),
                }
            })
            .to_vec();
        let origin = format!("resident:{epoch}")
            .parse()
            .expect("resident origin");
        let at = wire::Instant::new(jiff::Timestamp::now()).expect("fixture instant");
        let mut write =
            journal::JournalWrite::new(wire::Actor::Spice, origin, at, Producer::CommandDoor);
        let seated = write
            .fact(wire::body::UpSeated { epoch }, FactCause::Root)
            .expect("seating fact builds");
        write.residency_head(seated, move |_| wire::ResidencyHead {
            epoch,
            state: wire::ResidentState::Seated,
            topology_revision: 0,
            lanes,
            subscriptions: Vec::new(),
            renewals: Vec::new(),
        });
        commit(bus, write).expect("seating commits");
    }

    fn set_global(bus: &Bus, epoch: wire::CanonicalUuid, id: u8, fast_mode: bool) {
        let request = GlobalSettingsSet { fast_mode };
        let command = wire::Command {
            v: wire::WireVersion::CURRENT,
            id: uuid(id),
            epoch,
            kind: wire::CommandKind::GlobalSettingsSet,
            expected: Vec::new(),
            body: serde_json::to_value(&request).expect("settings intent encodes"),
        }
        .admit(
            &wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor: wire::Actor::TEST_PRINCIPAL,
                epoch,
                release: "1.2.3".parse().expect("fixture release"),
            },
            &PrincipalRoles::default(),
        )
        .expect("operator settings intent admits");
        let outcome = set(
            bus,
            epoch,
            &command,
            &request,
            &mut CommandEffects::default(),
        );
        assert!(
            matches!(
                outcome,
                Outcome::Reply(Reply {
                    position: ReplyPosition::Committed(_),
                    ..
                })
            ),
            "settings write commits: {outcome:?}"
        );
    }

    #[test]
    fn turning_global_fast_mode_on_forgets_every_seat_refusal() {
        let epoch = uuid(99);
        let bus = Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
            .expect("fixture bus starts");
        // Seats that still refuse, and seats whose next launch runs fast.
        let seats = || {
            let head = bus
                .residency_head()
                .expect("residency reads")
                .expect("residency exists");
            let head = head.body();
            let refusing = head.lanes.iter().filter(|row| row.refuses_fast_mode());
            let fast = head
                .lanes
                .iter()
                .filter(|row| lane_fast_mode(&bus, head, row).expect("fast mode resolves"));
            (refusing.count(), fast.count())
        };
        seat_refusals(&bus, epoch);

        set_global(&bus, epoch, 50, false);
        assert_eq!(
            seats(),
            (2, 0),
            "keeping the setting off keeps every refusal"
        );
        set_global(&bus, epoch, 51, true);
        assert_eq!(
            seats(),
            (0, 2),
            "turning it on retries fast mode on every seat"
        );

        seat_refusals(&bus, epoch);
        set_global(&bus, epoch, 52, true);
        assert_eq!(
            seats(),
            (2, 0),
            "writing on over on keeps refusals met since"
        );
        set_global(&bus, epoch, 53, false);
        assert_eq!(seats(), (2, 0), "turning it off keeps them");
        set_global(&bus, epoch, 54, true);
        assert_eq!(seats(), (0, 2), "turning it on again retries them");
    }
}
