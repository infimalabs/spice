//! Principal birth, profile, and role authority.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;

use wire::principal::{
    PrincipalAdmit, PrincipalDeauthorize, PrincipalDirectoryRead, PrincipalEdit, PrincipalHead,
    PrincipalRead, PrincipalRole, PrincipalRoleRef, PrincipalRoles,
};

use super::*;
use crate::resident::principal_surface;

const GIT_NAME: &str = "Ada Operator";
const GIT_EMAIL: &str = "ada@example.invalid";

struct Seated {
    repository: Repository,
    surface: crate::trust_command::Surface,
    paths: RuntimePaths,
    bus: Bus,
    /// Last, so it is removed only after the bus above has stopped its writer.
    _root: RemovedRoot,
}

/// Removes a fixture root when the test ends, passing or panicking.
struct RemovedRoot(PathBuf);

impl Drop for RemovedRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn seated(name: &str) -> Seated {
    let (root, paths) = resident_fixture(name);
    let root = RemovedRoot(root);
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(root.0.join(".git/config"))
            .expect("open fixture repository config"),
        "[user]\n\tname = {GIT_NAME}\n\temail = {GIT_EMAIL}"
    )
    .expect("configure fixture Git identity");
    let repository = Repository::discover(&root.0).expect("fixture repository resolves");
    let surface =
        crate::trust_command::Surface::load(&repository).expect("fixture trust surface loads");
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    commit_startup(&bus, &repository, &surface, test_uuid(1), None).expect("resident seats");
    Seated {
        repository,
        surface,
        paths,
        bus,
        _root: root,
    }
}

fn heads(bus: &Bus) -> Vec<PrincipalHead> {
    bus.read(|read| read.principal_heads())
        .expect("principal heads read")
        .into_iter()
        .map(journal::JournalHead::into_body)
        .collect()
}

fn command<B: serde::Serialize>(
    kind: CommandKind,
    body: &B,
    actor: Actor,
    id: u8,
) -> Result<wire::AdmittedCommand, wire::CommandRejection> {
    command_under(kind, body, actor, id, &PrincipalRoles::default())
}

/// Admit `body` as `actor` under exactly the role heads `roles`.
fn command_under<B: serde::Serialize>(
    kind: CommandKind,
    body: &B,
    actor: Actor,
    id: u8,
    roles: &PrincipalRoles,
) -> Result<wire::AdmittedCommand, wire::CommandRejection> {
    command_under_epoch(kind, body, actor, id, test_uuid(1), roles)
}

fn command_under_epoch<B: serde::Serialize>(
    kind: CommandKind,
    body: &B,
    actor: Actor,
    id: u8,
    epoch: wire::CanonicalUuid,
    roles: &PrincipalRoles,
) -> Result<wire::AdmittedCommand, wire::CommandRejection> {
    wire::Command {
        v: wire::WireVersion::CURRENT,
        id: test_uuid(id),
        epoch,
        kind,
        expected: Vec::new(),
        body: serde_json::to_value(body).expect("principal body encodes"),
    }
    .admit(
        &wire::Accepted {
            wire: wire::WireVersion::CURRENT,
            actor,
            epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        roles,
    )
}

/// Admit `body` as `principal`'s own human actor under the journal's
/// current role heads, as that principal's connection submits it.
fn as_principal<B: serde::Serialize>(
    seated: &Seated,
    principal: wire::CanonicalUuid,
    kind: CommandKind,
    body: &B,
    id: u8,
) -> wire::AdmittedCommand {
    as_principal_at_epoch(seated, principal, kind, body, id, test_uuid(1))
}

fn as_principal_at_epoch<B: serde::Serialize>(
    seated: &Seated,
    principal: wire::CanonicalUuid,
    kind: CommandKind,
    body: &B,
    id: u8,
    epoch: wire::CanonicalUuid,
) -> wire::AdmittedCommand {
    let roles = principal_surface::roles(&seated.bus).expect("role heads read");
    command_under_epoch(kind, body, Actor::Principal(principal), id, epoch, &roles)
        .expect("the principal's operator command admits")
}

/// Admit `body` as the bootstrap principal.
fn operator<B: serde::Serialize>(
    seated: &Seated,
    kind: CommandKind,
    body: &B,
    id: u8,
) -> wire::AdmittedCommand {
    operator_at_epoch(seated, kind, body, id, test_uuid(1))
}

fn operator_at_epoch<B: serde::Serialize>(
    seated: &Seated,
    kind: CommandKind,
    body: &B,
    id: u8,
    epoch: wire::CanonicalUuid,
) -> wire::AdmittedCommand {
    as_principal_at_epoch(
        seated,
        heads(&seated.bus)[0].principal,
        kind,
        body,
        id,
        epoch,
    )
}

fn role(
    seated: &Seated,
    kind: CommandKind,
    reference: PrincipalRoleRef,
    id: u8,
) -> Outcome<CommandAnswer> {
    let command = operator(seated, kind, &reference, id);
    principal_surface::role(&seated.bus, test_uuid(1), &command, &reference)
}

fn deauthorize(
    seated: &Seated,
    actor: wire::CanonicalUuid,
    principal: wire::CanonicalUuid,
    id: u8,
) -> Outcome<CommandAnswer> {
    let request = PrincipalDeauthorize { principal };
    let command = as_principal(
        seated,
        actor,
        CommandKind::PrincipalDeauthorize,
        &request,
        id,
    );
    principal_surface::deauthorize(&seated.bus, test_uuid(1), &command, &request)
}

fn refusal_reason(outcome: &Outcome<CommandAnswer>) -> &str {
    match outcome {
        Outcome::Refusal(refusal) => refusal.reason(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Admit `request` as the bootstrap principal, answering the admitted
/// principal and the capability its admission issued.
fn admitted(
    seated: &Seated,
    request: PrincipalAdmit,
    id: u8,
) -> (wire::CanonicalUuid, wire::CanonicalUuid) {
    let command = operator(seated, CommandKind::PrincipalAdmit, &request, id);
    match principal_surface::admit(
        &seated.bus,
        &seated.repository,
        test_uuid(1),
        &command,
        &request,
    ) {
        Outcome::Reply(Reply {
            body: CommandAnswer::Principal(standing),
            ..
        }) => (
            standing.principal,
            standing
                .capability
                .expect("an admission issues a capability"),
        ),
        other => panic!("expected an admission, got {other:?}"),
    }
}

fn bea(roles: &[PrincipalRole]) -> PrincipalAdmit {
    PrincipalAdmit {
        name: Some("Bea".to_owned()),
        email: None,
        roles: roles.iter().copied().collect(),
    }
}

#[test]
fn first_start_bears_one_git_profiled_operator_and_restarts_bear_nothing_more() {
    let seated = seated("principal-bootstrap");
    let [born] = heads(&seated.bus)
        .try_into()
        .expect("exactly one principal");
    assert!(born.holds(PrincipalRole::Operator));
    assert_eq!(born.profile.name.as_deref(), Some(GIT_NAME));
    assert_eq!(born.profile.email.as_deref(), Some(GIT_EMAIL));

    commit_startup(
        &seated.bus,
        &seated.repository,
        &seated.surface,
        test_uuid(9),
        None,
    )
    .expect("the resident restarts");
    assert_eq!(heads(&seated.bus), vec![born], "a restart bears nobody");
    let admissions = seated
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts read")
        .facts()
        .iter()
        .filter(|envelope| envelope.fact.kind() == wire::FactKind::PrincipalAdmitted)
        .count();
    assert_eq!(admissions, 1);
}

#[test]
fn a_connected_non_operator_reads_names_and_only_its_own_roles() {
    let seated = seated("principal-connected-directory");
    let bootstrap = Actor::Principal(
        heads(&seated.bus)
            .first()
            .expect("startup bears the bootstrap principal")
            .principal,
    );
    let (bea, _) = admitted(&seated, bea(&[]), 10);
    let roles = principal_surface::roles(&seated.bus).expect("role heads read");
    let actor = Actor::Principal(bea);

    command_under(
        CommandKind::PrincipalDirectoryRead,
        &PrincipalDirectoryRead {},
        actor.clone(),
        11,
        &roles,
    )
    .expect("connected principal directory read admits");
    assert!(
        command_under(
            CommandKind::PrincipalRead,
            &PrincipalRead {},
            actor.clone(),
            12,
            &roles,
        )
        .is_err(),
        "the operator-only administrative list remains refused"
    );

    let Outcome::Reply(Reply {
        body: CommandAnswer::PrincipalDirectory(answer),
        ..
    }) = principal_surface::directory(&seated.bus, &actor)
    else {
        panic!("directory read replies");
    };
    assert!(answer.roles.is_empty());
    assert_eq!(
        answer
            .principals
            .identity(&actor)
            .and_then(|identity| identity.name.as_deref()),
        Some("Bea")
    );
    assert_eq!(
        answer
            .principals
            .identity(&bootstrap)
            .and_then(|identity| identity.name.as_deref()),
        Some(GIT_NAME)
    );
}

#[test]
fn the_operator_role_never_leaves_the_last_connectable_principal_holding_it() {
    let seated = seated("principal-last-operator");
    let first = heads(&seated.bus)[0].principal;
    let revoke_first = PrincipalRoleRef {
        principal: first,
        role: PrincipalRole::Operator,
    };

    let refused = role(
        &seated,
        CommandKind::PrincipalRevoke,
        revoke_first.clone(),
        10,
    );
    assert_eq!(refusal_reason(&refused), "principal::last-operator");

    let (second, _) = admitted(&seated, bea(&[PrincipalRole::Operator]), 11);
    assert!(matches!(
        deauthorize(&seated, first, second, 12),
        Outcome::Reply(_)
    ));
    let before = heads(&seated.bus);
    let refused = role(&seated, CommandKind::PrincipalRevoke, revoke_first, 13);
    assert_eq!(
        refusal_reason(&refused),
        "principal::last-operator",
        "a principal no connection can act as leaves no one able to exercise the role"
    );
    assert_eq!(
        heads(&seated.bus),
        before,
        "a refused revocation writes nothing"
    );

    assert!(matches!(
        role(
            &seated,
            CommandKind::PrincipalRevoke,
            PrincipalRoleRef {
                principal: second,
                role: PrincipalRole::Operator,
            },
            14,
        ),
        Outcome::Reply(_)
    ));
    let after = heads(&seated.bus);
    assert!(
        after[1].roles.is_empty(),
        "the revoked holder keeps no role"
    );
    assert_eq!(
        (after[1].principal, after[1].born, &after[1].profile),
        (before[1].principal, before[1].born, &before[1].profile),
        "a role change moves only the role"
    );
    assert_eq!(after[0], before[0], "the other principal is untouched");
}

#[test]
fn an_operator_can_revoke_a_peer_born_under_an_earlier_resident_origin() {
    let seated = seated("principal-peer-revoke-after-restart");
    let first = heads(&seated.bus)[0].principal;
    let (peer, _) = admitted(&seated, bea(&[PrincipalRole::Operator]), 10);
    let born = heads(&seated.bus)[1].born;
    let original = seated
        .bus
        .read(move |read| read.room_fact(born))
        .expect("the peer admission reads")
        .expect("the peer admission exists")
        .origin;
    let epoch = test_uuid(2);
    commit_startup(
        &seated.bus,
        &seated.repository,
        &seated.surface,
        epoch,
        None,
    )
    .expect("the resident restarts");

    let request = PrincipalRoleRef {
        principal: peer,
        role: PrincipalRole::Operator,
    };
    let command = operator_at_epoch(&seated, CommandKind::PrincipalRevoke, &request, 11, epoch);
    assert!(matches!(
        principal_surface::role(&seated.bus, epoch, &command, &request),
        Outcome::Reply(_)
    ));

    let after = heads(&seated.bus);
    assert!(!after[1].holds(PrincipalRole::Operator));
    let revoked = seated
        .bus
        .read(|read| read.latest_fact(wire::FactKind::PrincipalRevoked))
        .expect("the revocation reads")
        .expect("the revocation exists");
    assert_eq!(revoked.origin, original);
    assert_eq!(revoked.actor, Actor::Principal(first));
}

/// Admission issues a capability the journal keeps only as a digest.
/// Deauthorization retires it for good, authored as the deauthorizing
/// principal, and never leaves the operator role without a connectable
/// holder.
#[test]
fn deauthorization_retires_an_issued_capability_but_never_the_last_connectable_operator() {
    let seated = seated("principal-deauthorize");
    let first = heads(&seated.bus)[0].principal;
    let (second, issued) = admitted(&seated, bea(&[]), 10);
    assert_eq!(
        heads(&seated.bus)[1].capability,
        Some(wire::principal::capability_digest(issued)),
        "the journal keeps only the issued capability's digest"
    );

    assert!(matches!(
        deauthorize(&seated, first, second, 11),
        Outcome::Reply(_)
    ));
    assert_eq!(heads(&seated.bus)[1].capability, None);
    let retired = seated
        .bus
        .read(|read| read.latest_fact(wire::FactKind::PrincipalDeauthorized))
        .expect("the journal reads")
        .expect("deauthorization writes its fact");
    assert_eq!(retired.actor, Actor::Principal(first));
    let retired = retired
        .fact
        .read::<wire::principal::PrincipalDeauthorized>()
        .expect("the fact decodes");
    assert_eq!(
        (retired.principal, retired.capability),
        (second, wire::principal::capability_digest(issued))
    );
    assert_eq!(
        refusal_reason(&deauthorize(&seated, first, second, 12)),
        "principal::capability-absent",
        "a retired capability is never retired twice or reissued"
    );

    let (third, _) = admitted(&seated, bea(&[PrincipalRole::Operator]), 13);
    let revoke_first = PrincipalRoleRef {
        principal: first,
        role: PrincipalRole::Operator,
    };
    assert!(
        matches!(
            role(&seated, CommandKind::PrincipalRevoke, revoke_first, 14),
            Outcome::Reply(_)
        ),
        "a connectable operator principal lets the first give the role up"
    );
    let before = heads(&seated.bus);
    assert_eq!(
        refusal_reason(&deauthorize(&seated, third, third, 15)),
        "principal::last-operator"
    );
    assert_eq!(heads(&seated.bus), before, "a refusal writes nothing");
}

#[test]
fn a_profile_edit_moves_only_its_own_profile_and_keeps_omitted_fields() {
    let seated = seated("principal-profile");
    let (other, _) = admitted(
        &seated,
        PrincipalAdmit {
            name: Some("Bea".to_owned()),
            email: Some("bea@example.invalid".to_owned()),
            roles: BTreeSet::new(),
        },
        10,
    );
    let before = heads(&seated.bus);
    let edit = PrincipalEdit {
        principal: before[0].principal,
        name: Some("Ada Renamed".to_owned()),
        email: None,
    };
    let command = operator(&seated, CommandKind::PrincipalEdit, &edit, 11);
    assert!(matches!(
        principal_surface::edit(&seated.bus, test_uuid(1), &command, &edit),
        Outcome::Reply(_)
    ));

    let after = heads(&seated.bus);
    assert_eq!(after[0].profile.name.as_deref(), Some("Ada Renamed"));
    assert_eq!(after[0].profile.email.as_deref(), Some(GIT_EMAIL));
    assert_eq!(
        (after[0].principal, after[0].born, &after[0].roles),
        (before[0].principal, before[0].born, &before[0].roles),
        "identity, birth, and roles never move with the profile"
    );
    assert_eq!(after[1].principal, other);
    assert_eq!(after[1], before[1], "the other principal is untouched");
}

#[test]
fn lanes_cannot_list_admit_profile_or_change_principal_roles() {
    let lane = Actor::Lane(test_uuid(2));
    let principal = test_uuid(5);
    let reference = PrincipalRoleRef {
        principal,
        role: PrincipalRole::Operator,
    };
    let refused = [
        command(
            CommandKind::PrincipalRead,
            &PrincipalRead {},
            lane.clone(),
            10,
        ),
        command(
            CommandKind::PrincipalAdmit,
            &PrincipalAdmit::default(),
            lane.clone(),
            11,
        ),
        command(
            CommandKind::PrincipalEdit,
            &PrincipalEdit {
                principal,
                name: Some("Lane".to_owned()),
                email: None,
            },
            lane.clone(),
            12,
        ),
        command(CommandKind::PrincipalGrant, &reference, lane.clone(), 13),
        command(CommandKind::PrincipalRevoke, &reference, lane, 14),
    ];
    for rejection in refused {
        let Err(wire::CommandRejection::Refusal(refusal)) = rejection else {
            panic!("a lane's principal command must refuse, got {rejection:?}");
        };
        assert_eq!(refusal.reason(), "command::authority");
    }
}

/// The command door admits each command under the generation's current role
/// book. Admitting another operator principal issues it a connectable
/// capability, so the bootstrap principal may give the role up, and the
/// revocation governs that principal's next command on the same connection.
#[test]
fn the_door_admits_each_command_under_the_roles_its_predecessor_left() {
    let seated = seated("principal-role-book");
    let startup = crate::resident::load_startup_surface(&seated.repository)
        .expect("fixture startup policy resolves");
    let capabilities = serve::axum::Capabilities::default();
    let discovery = serve::Discovery {
        endpoint: "http://127.0.0.1:1".to_owned(),
        capability: test_uuid(2),
        epoch: test_uuid(1),
    };
    let reads = read_materializations::CurrentReads::start_queueing(&seated.bus, &seated.paths);
    let reporter: Arc<dyn lane::capture::CaptureReporter> =
        Arc::new(super::startup::NoopCaptureReporter);
    let lane = lane_surface::Runtime::new(&capabilities, &discovery, reporter.clone(), &reads);
    lane.publish_roles(&seated.bus)
        .expect("the generation publishes its role heads");
    let resources = crate::board_command::resources::ResourceClaims::new(sill::LockTopology::new(
        seated.repository.worktree_root(),
        &startup.config.lock,
    ));
    let surface = command_surface::Surface::new(
        &seated.bus,
        &seated.repository,
        &seated.paths,
        test_uuid(1),
        command_surface::Runtimes::new(lane, lane_recovery::Runtime::new(reporter), &resources),
        &startup.config,
        &reads,
    );
    let bootstrap = heads(&seated.bus)[0].principal;
    let mut seats = Seats::new();
    let mut door = |kind, body: serde_json::Value, id| {
        let roles = capabilities.roles().current().expect("the role book reads");
        match command_under(kind, &body, Actor::Principal(bootstrap), id, &roles) {
            Ok(command) => match surface.dispatch_fresh(&mut seats, &command) {
                CommandDispatch::Complete { outcome, .. } => outcome,
                CommandDispatch::Launch(_) => panic!("a principal command launches nothing"),
            },
            Err(wire::CommandRejection::Refusal(refusal)) => Outcome::Refusal(refusal),
            Err(other) => panic!("expected admission or refusal, got {other:?}"),
        }
    };
    let read = || serde_json::to_value(PrincipalRead {}).expect("read encodes");
    assert!(matches!(
        door(CommandKind::PrincipalRead, read(), 10),
        Outcome::Reply(_)
    ));
    assert!(matches!(
        door(
            CommandKind::PrincipalAdmit,
            serde_json::to_value(bea(&[PrincipalRole::Operator])).expect("admission encodes"),
            11
        ),
        Outcome::Reply(_)
    ));
    let revoke = PrincipalRoleRef {
        principal: bootstrap,
        role: PrincipalRole::Operator,
    };
    assert!(matches!(
        door(
            CommandKind::PrincipalRevoke,
            serde_json::to_value(revoke).expect("revocation encodes"),
            12,
        ),
        Outcome::Reply(_)
    ));
    assert_eq!(
        refusal_reason(&door(CommandKind::PrincipalRead, read(), 13)),
        "command::authority",
        "the revocation governs the principal's next command on the same connection"
    );
}
