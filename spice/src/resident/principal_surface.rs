//! Durable principals: the operator's admission, profile, and role commands,
//! and the first start's birth of the repository owner.
//!
//! Every write re-proves, inside its transaction, the head it was computed
//! from, so concurrent edits refuse rather than overwrite one another.

use std::collections::BTreeSet;

use bus::{Bus, JournalCommit};
use journal::{BuildError, FactCause, GuardError, JournalGuard, JournalWrite};
use spice::repository::Repository;
use wire::intent::CommandAnswer;
use wire::principal::{
    PrincipalAdmit, PrincipalAdmitted, PrincipalDeauthorize, PrincipalDeauthorized,
    PrincipalDirectory, PrincipalDirectoryAnswer, PrincipalEdit, PrincipalGranted, PrincipalHead,
    PrincipalListing, PrincipalProfile, PrincipalProfiled, PrincipalRevoked, PrincipalRole,
    PrincipalRoleRef, PrincipalRoles, PrincipalStanding, PrincipalsAnswer,
};
use wire::{
    Actor, CanonicalUuid, CommandKind, Outcome, Producer, Refusal, RefusalDiagnostic, Reply,
    ReplyPosition, Seq, SeqRange,
};

use super::settings_surface::{git_profile, malformed, normalize};
use super::{
    CommandResult, commit, new_uuid, seq, store_fault, typed_command, typed_outcome, write,
    write_at_origin,
};

/// Every principal's display identity and only the reader's current roles.
pub(super) fn directory(bus: &Bus, actor: &Actor) -> Outcome<CommandAnswer> {
    let (cursor, heads) = match bus.read(|read| {
        Ok((
            read.cursor()?,
            read.principal_heads()?
                .into_iter()
                .map(journal::JournalHead::into_body)
                .collect::<Vec<_>>(),
        ))
    }) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return store_fault("principal::journal-unreadable", &error.to_string());
        }
    };
    let Some(cursor) = cursor else {
        return store_fault(
            "principal::journal-empty",
            "a running resident has no startup authority fact",
        );
    };
    let roles = PrincipalRoles::new(heads.clone())
        .roles(actor)
        .unwrap_or_default();
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(cursor),
        body: CommandAnswer::PrincipalDirectory(PrincipalDirectoryAnswer {
            principals: PrincipalDirectory::display(&heads),
            roles,
        }),
    })
}

/// Every born principal in birth order at one immutable journal horizon, each
/// with the commit instant of the admission that bore it.
pub(super) fn read(bus: &Bus) -> Outcome<CommandAnswer> {
    let snapshot = bus.read(|read| {
        let cursor = read.cursor()?;
        let mut principals = Vec::new();
        for head in read.principal_heads()? {
            let head = head.into_body();
            let Some(birth) = read.room_fact(head.born)? else {
                return Err(GuardError::Invalid(format!(
                    "principal {} names birth {} beyond the read horizon",
                    head.principal, head.born
                )));
            };
            principals.push(PrincipalListing {
                admitted: birth.at,
                head,
            });
        }
        Ok((cursor, principals))
    });
    let (cursor, principals) = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return store_fault("principal::journal-unreadable", &error.to_string());
        }
    };
    let Some(cursor) = cursor else {
        return store_fault(
            "principal::journal-empty",
            "a running resident has no startup authority fact",
        );
    };
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(cursor),
        body: CommandAnswer::Principals(PrincipalsAnswer { principals }),
    })
}

/// Bear one principal with the given or Git-copied profile, issuing the
/// capability it authenticates through. The reply hands that capability off
/// once; the journal keeps only its digest.
pub(super) fn admit(
    bus: &Bus,
    repository: &Repository,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    request: &PrincipalAdmit,
) -> Outcome<CommandAnswer> {
    let profile = if request.name.is_none() && request.email.is_none() {
        match git_profile(repository) {
            Ok(profile) => profile,
            Err(outcome) => return typed_outcome(outcome),
        }
    } else {
        match given_profile(request.name.as_deref(), request.email.as_deref()) {
            Ok(profile) => profile,
            Err(refusal) => return refusal,
        }
    };
    let standing = PrincipalStanding {
        principal: new_uuid(),
        profile,
        roles: request.roles.clone(),
        capability: Some(new_uuid()),
    };
    let mut proposal = match write(command.actor().clone(), epoch, Producer::CommandDoor) {
        Ok(proposal) => proposal,
        Err(outcome) => return typed_outcome(outcome),
    };
    if let Err(error) = birth(&mut proposal, &standing, FactCause::Root) {
        return store_fault("principal::fact-invalid", &error.to_string());
    }
    finish(bus, proposal, epoch, command, request.clone(), standing)
}

/// Replace the given fields of one principal's profile; an absent field keeps
/// its value and a blank one clears it.
pub(super) fn edit(
    bus: &Bus,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    request: &PrincipalEdit,
) -> Outcome<CommandAnswer> {
    let given = match given_profile(request.name.as_deref(), request.email.as_deref()) {
        Ok(given) => given,
        Err(refusal) => return refusal,
    };
    let (position, origin, head) = match current(bus, request.principal) {
        Ok(current) => current,
        Err(outcome) => return outcome,
    };
    let profile = PrincipalProfile {
        name: if request.name.is_some() {
            given.name
        } else {
            head.profile.name.clone()
        },
        email: if request.email.is_some() {
            given.email
        } else {
            head.profile.email.clone()
        },
    };
    if profile == head.profile {
        return refused(
            "principal::profile-unchanged",
            request.principal,
            "a profile differing from the recorded one",
            None,
        );
    }
    let standing = PrincipalStanding {
        principal: head.principal,
        profile: profile.clone(),
        roles: head.roles.clone(),
        capability: None,
    };
    let mut proposal = match write_at_origin(command.actor().clone(), origin, Producer::CommandDoor)
    {
        Ok(proposal) => proposal,
        Err(outcome) => return typed_outcome(outcome),
    };
    proposal.guard(unmoved(head.principal, position));
    let fact = match proposal.fact(
        PrincipalProfiled {
            principal: head.principal,
            from: head.profile.clone(),
            profile: profile.clone(),
        },
        FactCause::Existing(position),
    ) {
        Ok(fact) => fact,
        Err(error) => return store_fault("principal::fact-invalid", &error.to_string()),
    };
    proposal.principal_head(fact, move |_| PrincipalHead { profile, ..head });
    finish(bus, proposal, epoch, command, request.clone(), standing)
}

/// Grant or revoke one role. The operator role never leaves the last actor
/// able to exercise it.
pub(super) fn role(
    bus: &Bus,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    request: &PrincipalRoleRef,
) -> Outcome<CommandAnswer> {
    let grant = command.kind() == CommandKind::PrincipalGrant;
    let (principal, role) = (request.principal, request.role);
    let (position, origin, head) = match current(bus, principal) {
        Ok(current) => current,
        Err(outcome) => return outcome,
    };
    if grant == head.holds(role) {
        let (reason, expected) = if grant {
            ("principal::role-held", "a role the principal does not hold")
        } else {
            ("principal::role-absent", "a role the principal holds")
        };
        return refused(reason, principal, expected, Some(role.as_str().to_owned()));
    }
    let last = !grant && role == PrincipalRole::Operator;
    let mut roles = head.roles.clone();
    if grant {
        roles.insert(role);
    } else {
        roles.remove(&role);
    }
    let standing = PrincipalStanding {
        principal,
        profile: head.profile.clone(),
        roles: roles.clone(),
        capability: None,
    };
    let mut proposal = match write_at_origin(command.actor().clone(), origin, Producer::CommandDoor)
    {
        Ok(proposal) => proposal,
        Err(outcome) => return typed_outcome(outcome),
    };
    proposal.guard(unmoved(principal, position));
    if last
        && let Err(refusal) = keep_an_operator(bus, &mut proposal, principal, |head| {
            head.roles.remove(&PrincipalRole::Operator);
        })
    {
        return refusal;
    }
    let cause = FactCause::Existing(position);
    let fact = if grant {
        proposal.fact(PrincipalGranted { principal, role }, cause)
    } else {
        proposal.fact(PrincipalRevoked { principal, role }, cause)
    };
    let fact = match fact {
        Ok(fact) => fact,
        Err(error) => return store_fault("principal::fact-invalid", &error.to_string()),
    };
    proposal.principal_head(fact, move |_| PrincipalHead { roles, ..head });
    finish(bus, proposal, epoch, command, request.clone(), standing)
}

/// Retire one principal's issued capability, ending its open sessions. The
/// operator role never leaves the last actor able to exercise it.
pub(super) fn deauthorize(
    bus: &Bus,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    request: &PrincipalDeauthorize,
) -> Outcome<CommandAnswer> {
    let principal = request.principal;
    let (position, origin, head) = match current(bus, principal) {
        Ok(current) => current,
        Err(outcome) => return outcome,
    };
    let Some(capability) = head.capability.clone() else {
        return refused(
            "principal::capability-absent",
            principal,
            "a principal holding an issued capability",
            None,
        );
    };
    let standing = PrincipalStanding {
        principal,
        profile: head.profile.clone(),
        roles: head.roles.clone(),
        capability: None,
    };
    let mut proposal = match write_at_origin(command.actor().clone(), origin, Producer::CommandDoor)
    {
        Ok(proposal) => proposal,
        Err(outcome) => return typed_outcome(outcome),
    };
    proposal.guard(unmoved(principal, position));
    if head.holds(PrincipalRole::Operator)
        && let Err(refusal) = keep_an_operator(bus, &mut proposal, principal, |head| {
            head.capability = None;
        })
    {
        return refusal;
    }
    let fact = match proposal.fact(
        PrincipalDeauthorized {
            principal,
            capability,
        },
        FactCause::Existing(position),
    ) {
        Ok(fact) => fact,
        Err(error) => return store_fault("principal::fact-invalid", &error.to_string()),
    };
    proposal.principal_head(fact, move |_| PrincipalHead {
        capability: None,
        ..head
    });
    finish(bus, proposal, epoch, command, request.clone(), standing)
}

/// The current role heads every principal authority decision reads.
pub(super) fn roles(bus: &Bus) -> CommandResult<wire::principal::PrincipalRoles> {
    bus.read(|read| read.principal_roles())
        .map_err(|error| store_fault("principal::journal-unreadable", &error.to_string()))
}

/// On a journal holding no principal, admit the repository identity from Git
/// as the first operator principal. Any born principal makes this a no-op, so
/// restarts add nothing.
pub(super) fn bootstrap(
    bus: &Bus,
    repository: &Repository,
    epoch: CanonicalUuid,
    cause: FactCause,
) -> CommandResult<Option<JournalCommit>> {
    let born = bus
        .read(|read| Ok(!read.principal_heads()?.is_empty()))
        .map_err(|error| store_fault("principal::journal-unreadable", &error.to_string()))?;
    if born {
        return Ok(None);
    }
    let standing = PrincipalStanding {
        principal: new_uuid(),
        profile: git_profile(repository)?,
        roles: BTreeSet::from([PrincipalRole::Operator]),
        capability: None,
    };
    let mut pending = write(Actor::Spice, epoch, Producer::CommandDoor)?;
    pending.guard(JournalGuard::new(|read| {
        if read.principal_heads()?.is_empty() {
            Ok(())
        } else {
            Err(GuardError::refused("principal::already-born"))
        }
    }));
    birth(&mut pending, &standing, cause)
        .map_err(|error| store_fault("principal::fact-invalid", &error.to_string()))?;
    commit(bus, pending).map(Some)
}

const LAST_OPERATOR: &str = "principal::last-operator";

fn birth(
    proposal: &mut JournalWrite,
    standing: &PrincipalStanding,
    cause: FactCause,
) -> Result<(), BuildError> {
    let PrincipalStanding {
        principal,
        profile,
        roles,
        capability,
    } = standing.clone();
    let capability = capability.map(wire::principal::capability_digest);
    let fact = proposal.fact(
        PrincipalAdmitted {
            principal,
            profile: profile.clone(),
            roles: roles.clone(),
            capability: capability.clone(),
        },
        cause,
    )?;
    proposal.principal_head(fact, move |born| PrincipalHead {
        principal,
        born,
        profile,
        roles,
        capability,
    });
    Ok(())
}

fn given_profile(
    name: Option<&str>,
    email: Option<&str>,
) -> Result<PrincipalProfile, Outcome<CommandAnswer>> {
    for (field, value) in [("name", name), ("email", email)] {
        if malformed(value) {
            return Err(Outcome::Refusal(Refusal::bare(RefusalDiagnostic::new(
                "principal::profile-malformed",
                Some(field.to_owned()),
                Some("one line without control characters".to_owned()),
                None,
            ))));
        }
    }
    Ok(PrincipalProfile {
        name: normalize(name),
        email: normalize(email),
    })
}

fn current(
    bus: &Bus,
    principal: CanonicalUuid,
) -> Result<(Seq, wire::Origin, PrincipalHead), Outcome<CommandAnswer>> {
    match bus.read(move |read| {
        let Some(head) = read.principal_head(principal)? else {
            return Ok(None);
        };
        let position = head.position();
        let origin = read
            .room_fact(position)?
            .ok_or_else(|| {
                GuardError::Invalid(format!(
                    "principal {principal} names head {position} beyond the read horizon"
                ))
            })?
            .origin;
        Ok(Some((position, origin, head.into_body())))
    }) {
        Ok(Some(current)) => Ok(current),
        Ok(None) => Err(refused(
            "principal::missing",
            principal,
            "a born principal",
            None,
        )),
        Err(error) => Err(store_fault(
            "principal::journal-unreadable",
            &error.to_string(),
        )),
    }
}

/// Refuse, now and again inside the write, a change to `principal`'s head
/// that would leave no actor able to exercise the operator role.
fn keep_an_operator(
    bus: &Bus,
    proposal: &mut JournalWrite,
    principal: CanonicalUuid,
    change: fn(&mut PrincipalHead),
) -> Result<(), Outcome<CommandAnswer>> {
    match bus.read(move |read| still_operating(read, principal, change)) {
        Ok(true) => {}
        Ok(false) => return Err(last_operator(principal)),
        Err(error) => {
            return Err(store_fault(
                "principal::journal-unreadable",
                &error.to_string(),
            ));
        }
    }
    proposal.guard(JournalGuard::new(move |read| {
        if still_operating(read, principal, change)? {
            Ok(())
        } else {
            Err(GuardError::refused(LAST_OPERATOR))
        }
    }));
    Ok(())
}

/// Whether some actor would still hold the operator role through the
/// authority predicate once `change` applies to `principal`'s head. Only the
/// bootstrap principal, through the local owner's capability, and a principal
/// holding an issued capability can connect; a principal no one can act as
/// keeps no one able to exercise the role.
fn still_operating(
    read: &journal::JournalRead<'_>,
    principal: CanonicalUuid,
    change: fn(&mut PrincipalHead),
) -> Result<bool, GuardError> {
    let mut heads: Vec<PrincipalHead> = read
        .principal_heads()?
        .into_iter()
        .map(journal::JournalHead::into_body)
        .collect();
    heads.sort_by_key(|head| head.born);
    for (index, head) in heads.iter_mut().enumerate() {
        if head.principal == principal {
            change(head);
        }
        if index > 0 && head.capability.is_none() {
            head.roles.clear();
        }
    }
    Ok(!PrincipalRoles::new(heads)
        .holders(PrincipalRole::Operator)
        .is_empty())
}

fn unmoved(principal: CanonicalUuid, position: Seq) -> JournalGuard {
    JournalGuard::new(move |read| match read.principal_head(principal)? {
        Some(head) if head.position() == position => Ok(()),
        _ => Err(GuardError::refused("principal::moved")),
    })
}

fn last_operator(principal: CanonicalUuid) -> Outcome<CommandAnswer> {
    refused(
        LAST_OPERATOR,
        principal,
        "another actor holding the operator role",
        Some("none".to_owned()),
    )
}

fn refused(
    reason: &str,
    principal: CanonicalUuid,
    expected: &str,
    observed: Option<String>,
) -> Outcome<CommandAnswer> {
    Outcome::Refusal(Refusal::bare(RefusalDiagnostic::new(
        reason,
        Some(principal.to_string()),
        Some(expected.to_owned()),
        observed,
    )))
}

fn finish<B>(
    bus: &Bus,
    mut proposal: JournalWrite,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    request: B,
    standing: PrincipalStanding,
) -> Outcome<CommandAnswer>
where
    B: serde::Serialize,
{
    let receipt = PrincipalStanding {
        capability: None,
        ..standing.clone()
    };
    if let Err(error) = proposal.command_result(typed_command(command, request), epoch, receipt) {
        return store_fault("principal::receipt-invalid", &error.to_string());
    }
    match commit(bus, proposal) {
        Ok(committed) => Outcome::Reply(Reply {
            position: ReplyPosition::Committed(SeqRange {
                first: seq(committed.first()),
                last: seq(committed.last()),
            }),
            body: CommandAnswer::Principal(Box::new(standing)),
        }),
        Err(outcome) => typed_outcome(outcome),
    }
}
