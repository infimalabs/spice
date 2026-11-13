//! CLI acquisition and typed resident mappings for lane lifecycle commands.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use clap::ArgMatches;
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::intent::{
    CommandAnswer, LaneAction, LaneAnswer, LaneAnswerRow, LaneEffortRetune, LaneFastRetune,
    LaneRecall, LaneRenewal, LaneRetune, LaneSeat, LaneState, TextAnswer,
};
use wire::{CanonicalUuid, Outcome, Refusal, Reply};

use crate::cli::Contract;
use crate::command_client::{self, Host, SelectedCommand};
use crate::{environment_fault, protocol_fault};

mod list;
pub(crate) mod purge;

/// Execute `lane list`, whose text and JSON forms carry the same rows.
pub(crate) fn list(
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<list::LanePage> {
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(error) => return environment_fault("lane::cwd-unavailable", error.to_string()),
    };
    match Repository::discover(&current) {
        Ok(repository) => list_at(&repository, matches, contract, ambient),
        Err(error) => environment_fault(spice::repository::ResolveError::REASON, error.to_string()),
    }
}

/// [`list`] against `repository`.
pub(crate) fn list_at(
    repository: &Repository,
    matches: &ArgMatches,
    contract: &Contract,
    ambient: &Ambient,
) -> Outcome<list::LanePage> {
    list::run(repository, matches, contract, ambient)
}

/// Execute one landed lane-family leaf.
pub(crate) fn run(
    matches: &ArgMatches,
    contract: &Contract,
    configuration: Option<&spice::config::RepositoryCapture>,
    ambient: &Ambient,
) -> Outcome<CommandAnswer> {
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(error) => return environment_fault("lane::cwd-unavailable", error.to_string()),
    };
    let repository = match Repository::discover(&current) {
        Ok(repository) => repository,
        Err(error) => {
            return environment_fault(spice::repository::ResolveError::REASON, error.to_string());
        }
    };
    let state_paths = match repository.admit_state() {
        Ok(state_paths) => state_paths,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let serve = state_paths.serve();
    let host = Host::new(&repository, ambient);

    match contract.path {
        ["lane"] => command_client::execute(serve, contract, SelectedCommand::LaneRead, ambient),
        ["lane", "add"] => add(
            &repository,
            serve,
            &current,
            matches,
            contract,
            configuration,
            ambient,
        ),
        ["lane", "drain"] => state(host, serve, matches, contract, LaneAction::Drain),
        ["lane", "wake"] => state(host, serve, matches, contract, LaneAction::Wake),
        ["lane", "model"] => model(host, serve, matches, contract),
        ["lane", "effort"] => effort(host, serve, matches, contract),
        ["lane", "fast"] => fast(host, serve, matches, contract),
        ["lane", "renew"] => renew(host, serve, matches, contract),
        ["lane", "remove"] => state(host, serve, matches, contract, LaneAction::Remove),
        ["lane", "import"] => import(host, serve, &current, matches, contract),
        ["recall"] => recall(host, serve, matches, contract),
        _ => protocol_fault(
            "lane::dispatch-gap",
            "lane command runner received a contract outside its closed family",
        ),
    }
}

fn add(
    repository: &Repository,
    serve: &Path,
    current: &Path,
    matches: &ArgMatches,
    contract: &Contract,
    configuration: Option<&spice::config::RepositoryCapture>,
    ambient: &Ambient,
) -> Outcome<CommandAnswer> {
    let requested = matches
        .get_one::<String>("WORKTREE")
        .map_or_else(|| repository.worktree_root().to_path_buf(), PathBuf::from);
    let requested = if requested.is_absolute() {
        requested
    } else {
        current.join(requested)
    };
    let worktree = match Repository::discover(&requested) {
        Ok(worktree) => worktree,
        Err(error) => return environment_fault("lane::worktree-unresolved", error.to_string()),
    };
    if worktree.common_dir() != repository.common_dir() {
        return refusal(
            "lane::foreign-worktree",
            Some(repository.common_dir().display().to_string()),
            Some(worktree.common_dir().display().to_string()),
            "choose an existing worktree belonging to the current Git common directory",
        );
    }
    let requested = match std::fs::canonicalize(&requested) {
        Ok(requested) => requested,
        Err(error) => return environment_fault("lane::worktree-unresolved", error.to_string()),
    };
    if requested != worktree.worktree_root() {
        return refusal(
            "lane::worktree-not-root",
            Some(worktree.worktree_root().display().to_string()),
            Some(requested.display().to_string()),
            "name the root of one existing worktree, not a path nested beneath it",
        );
    }
    let loaded = match spice::config::capture_repository_scoped(&worktree, &[], configuration) {
        Ok(loaded) => loaded,
        Err(error) => return configuration_refusal(&error),
    };
    let resolved = loaded.resolved();
    let life = matches.get_one::<String>("life").map_or_else(
        || resolved.root().lane().life().as_str().to_owned(),
        |life| life.to_ascii_lowercase(),
    );
    let vendor = match resolve_vendor(
        matches.get_one::<String>("vendor").map(String::as_str),
        ambient
            .get(wire::SpiceVariable::AgentVendor.as_str())
            .map(OsStr::to_os_string),
        resolved.root().lane().vendor(),
    ) {
        Ok(vendor) => vendor,
        Err(VendorResolutionError::EnvironmentEncoding) => {
            return protocol_fault(
                "lane::vendor-environment-invalid",
                "SPICE_AGENT_VENDOR must be valid UTF-8",
            );
        }
        Err(VendorResolutionError::Invalid(detail)) => {
            return protocol_fault("lane::vendor-invalid", detail);
        }
    };
    let lane = new_uuid();
    let team = match matches.get_one::<String>("team") {
        Some(team) => match team.parse::<wire::CanonicalUuid>() {
            Ok(team) => team,
            Err(error) => return protocol_fault("lane::team-invalid", error.to_string()),
        },
        None => lane,
    };
    let topology_revision = match observe_topology(serve, ambient) {
        Ok(revision) => revision,
        Err(outcome) => return outcome,
    };
    let body = lane_seat_body(
        matches,
        topology_revision,
        lane,
        team,
        worktree.worktree_root().display().to_string(),
        life,
        vendor,
    );
    command_client::execute(
        serve,
        contract,
        SelectedCommand::LaneSeat { body: &body },
        ambient,
    )
}

fn lane_seat_body(
    matches: &ArgMatches,
    topology_revision: u64,
    lane: CanonicalUuid,
    team: CanonicalUuid,
    worktree: String,
    life: String,
    vendor: wire::Name,
) -> LaneSeat {
    LaneSeat {
        topology_revision,
        lane,
        team,
        name: matches
            .get_one::<String>("name")
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty()),
        worktree,
        life,
        vendor,
        model: matches
            .get_one::<String>("model")
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty()),
        effort: matches.get_one::<String>("effort").cloned(),
        resume: None,
    }
}

fn observe_topology(serve: &Path, ambient: &Ambient) -> Result<u64, Outcome<CommandAnswer>> {
    read_lanes(serve, ambient).map(|reply| reply.body.topology_revision)
}

/// The lane the command's required `ID` selector names.
fn lane_id(host: Host<'_>, matches: &ArgMatches) -> Result<CanonicalUuid, Outcome<CommandAnswer>> {
    let Some(id) = matches.get_one::<String>("ID") else {
        return Err(protocol_fault(
            "lane::id-missing",
            "generated parser omitted required lane ID",
        ));
    };
    crate::lane_selector::resolve(host, id, "lane::id-invalid")
}

/// The lane a retune names and the topology revision it is checked against.
fn retune_target(
    host: Host<'_>,
    serve: &Path,
    matches: &ArgMatches,
) -> Result<(CanonicalUuid, u64), Outcome<CommandAnswer>> {
    Ok((
        lane_id(host, matches)?,
        observe_topology(serve, host.ambient)?,
    ))
}

/// The selected lane, its current team, and the topology revision one renewal
/// request witnesses from the same lane-table read.
fn renewal_target(
    host: Host<'_>,
    serve: &Path,
    matches: &ArgMatches,
) -> Result<(CanonicalUuid, CanonicalUuid, u64), Outcome<CommandAnswer>> {
    let lane = lane_id(host, matches)?;
    let lanes = read_lanes(serve, host.ambient)?;
    let team = lanes
        .body
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .map(|row| row.team)
        .ok_or_else(|| missing_lane(lane))?;
    Ok((lane, team, lanes.body.topology_revision))
}

fn missing_lane<B>(lane: CanonicalUuid) -> Outcome<B> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            "lane::missing".to_owned(),
            Some("lane".to_owned()),
            Some("one current lane".to_owned()),
            Some(lane.to_string()),
        )
        .with_detail("read lane and retry with a current lane identity"),
    ))
}

/// Read the resident's current lane table under the lane-read contract.
fn read_lanes<B>(serve: &Path, ambient: &Ambient) -> Result<Reply<Box<LaneAnswer>>, Outcome<B>> {
    let contract = crate::cli::contract(&["lane"])
        .expect("the generated CLI registry carries the lane-read contract");
    match command_client::execute(serve, contract, SelectedCommand::LaneRead, ambient) {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::Lane(body),
        }) => Ok(Reply { position, body }),
        Outcome::Reply(_) => Err(protocol_fault(
            "lane::answer-invalid",
            "resident lane read returned a non-lane answer",
        )),
        Outcome::Refusal(refusal) => Err(Outcome::Refusal(refusal)),
        Outcome::Fault(fault) => Err(Outcome::Fault(fault)),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum VendorResolutionError {
    EnvironmentEncoding,
    Invalid(String),
}

fn resolve_vendor(
    explicit: Option<&str>,
    environment: Option<OsString>,
    configured: &wire::Name,
) -> Result<wire::Name, VendorResolutionError> {
    if let Some(explicit) = explicit {
        return explicit
            .parse()
            .map_err(|error: wire::NameError| VendorResolutionError::Invalid(error.to_string()));
    }
    let environment = environment
        .map(OsString::into_string)
        .transpose()
        .map_err(|_| VendorResolutionError::EnvironmentEncoding)?
        .filter(|value| !value.trim().is_empty());
    environment.map_or_else(
        || Ok(configured.clone()),
        |value| {
            value
                .trim()
                .to_ascii_lowercase()
                .parse()
                .map_err(|error: wire::NameError| VendorResolutionError::Invalid(error.to_string()))
        },
    )
}

fn configuration_refusal(error: &spice::config::LoadError) -> Outcome<CommandAnswer> {
    Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
        "lane::configuration-refused".to_owned(),
        Some("effective-configuration".to_owned()),
        Some("tracked and local inputs admitted by the configuration registry".to_owned()),
        Some(error.diagnostic()),
    )))
}

fn state(
    host: Host<'_>,
    serve: &Path,
    matches: &ArgMatches,
    contract: &Contract,
    action: LaneAction,
) -> Outcome<CommandAnswer> {
    let lane = match lane_id(host, matches) {
        Ok(lane) => lane,
        Err(outcome) => return outcome,
    };
    let body = LaneState {
        lane,
        action,
        worktree: None,
    };
    command_client::execute(
        serve,
        contract,
        SelectedCommand::LaneState { body: &body },
        host.ambient,
    )
}

/// What one `lane model` or `lane effort` invocation asks of a seat.
#[derive(Debug, PartialEq, Eq)]
enum SeatEdit {
    /// Report the seat's setting and change nothing.
    Read,
    /// Pin the seat to this spelling.
    Set(String),
    /// Return the seat to the value its vendor is configured to launch with.
    Clear,
}

/// The seat edit named by the optional `value` argument and the `--clear`
/// flag: a value sets, `--clear` clears, neither reads, and both are refused.
fn seat_edit(matches: &ArgMatches, value: &str) -> Result<SeatEdit, Outcome<CommandAnswer>> {
    let named = matches
        .get_one::<String>(value)
        .map(|named| named.trim().to_owned());
    match (named, matches.get_flag("clear")) {
        (Some(named), false) => Ok(SeatEdit::Set(named)),
        (None, true) => Ok(SeatEdit::Clear),
        (None, false) => Ok(SeatEdit::Read),
        (Some(named), true) => Err(Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "lane::clear-conflict".to_owned(),
                Some("clear".to_owned()),
                Some(format!("either {value} or --clear")),
                Some(named),
            )
            .with_detail("name a value to pin the seat or --clear to follow the vendor, not both"),
        ))),
    }
}

/// Which seat setting a read reports.
#[derive(Clone, Copy)]
enum SeatSetting {
    Model,
    Effort,
}

impl SeatSetting {
    const fn noun(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Effort => "effort",
        }
    }
}

/// Report the model or effort one lane's seat holds for future launches and
/// where it comes from, without sending a live command: the seat's own override,
/// or the default its vendor launches with under the lane worktree's
/// configuration.
fn seat_read(
    host: Host<'_>,
    serve: &Path,
    matches: &ArgMatches,
    setting: SeatSetting,
) -> Outcome<CommandAnswer> {
    let lane = match lane_id(host, matches) {
        Ok(lane) => lane,
        Err(outcome) => return outcome,
    };
    let Reply { position, body } = match read_lanes(serve, host.ambient) {
        Ok(reply) => reply,
        Err(outcome) => return outcome,
    };
    let Some(row) = body.lanes.iter().find(|row| row.lane == lane) else {
        return Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "lane::missing".to_owned(),
                Some("lane".to_owned()),
                Some("one current lane".to_owned()),
                Some(lane.to_string()),
            )
            .with_detail("read lane and retry with a current lane identity"),
        ));
    };
    let pinned = match setting {
        SeatSetting::Model => row.seat_model.as_deref(),
        SeatSetting::Effort => row.seat_effort.as_deref(),
    };
    let text = match pinned {
        Some(pinned) => format!("{pinned} (lane override)"),
        None => match vendor_default(row, setting) {
            Ok(Some(default)) => format!("{default} (vendor default)"),
            Ok(None) => format!(
                "vendor default ({} declares no {})",
                row.vendor,
                setting.noun()
            ),
            Err(outcome) => return outcome,
        },
    };
    Outcome::Reply(Reply {
        position,
        body: CommandAnswer::Text(TextAnswer { text }),
    })
}

/// The model or effort a seat without an override launches with, resolved as
/// the launch does: the lane's vendor under its own worktree's configuration.
/// A vendor with no compiled declaration, such as the demo's simulated one,
/// never launches and so has no default to name.
fn vendor_default(
    row: &LaneAnswerRow,
    setting: SeatSetting,
) -> Result<Option<String>, Outcome<CommandAnswer>> {
    let Some(vendor) = spice::extension::VENDORS
        .iter()
        .find(|entry| entry.vendor.name() == row.vendor.as_str())
        .map(|entry| entry.vendor)
    else {
        return Ok(None);
    };
    let repository = Repository::discover(Path::new(&row.worktree))
        .map_err(|error| environment_fault("lane::worktree-unresolved", error.to_string()))?;
    let resolved = spice::config::load_repository(&repository, &[])
        .map_err(|error| configuration_refusal(&error))?;
    let launch = vendor.launch_values(resolved.root());
    Ok(Some(match setting {
        SeatSetting::Model => launch.model.to_owned(),
        SeatSetting::Effort => launch.effort.as_str().to_owned(),
    }))
}

/// Read, pin, or clear the model spelling one seated lane's future launches run.
///
/// A pin also retunes a running OMP occupant after resolving the spelling
/// against that occupant's served catalog. Naming no spelling reads the seat.
/// `--clear` returns future launches to the vendor's configured model; the
/// request states the seat outright rather than describing a change to it.
fn model(
    host: Host<'_>,
    serve: &Path,
    matches: &ArgMatches,
    contract: &Contract,
) -> Outcome<CommandAnswer> {
    let model = match seat_edit(matches, "MODEL") {
        Ok(SeatEdit::Read) => return seat_read(host, serve, matches, SeatSetting::Model),
        Ok(SeatEdit::Set(model)) => Some(model),
        Ok(SeatEdit::Clear) => None,
        Err(outcome) => return outcome,
    };
    let (lane, topology_revision) = match retune_target(host, serve, matches) {
        Ok(target) => target,
        Err(outcome) => return outcome,
    };
    let body = LaneRetune {
        topology_revision,
        lane,
        model,
    };
    command_client::execute(
        serve,
        contract,
        SelectedCommand::LaneRetune { body: &body },
        host.ambient,
    )
}
/// Read, pin, or clear the reasoning effort one seated lane's next launch runs.
///
/// Naming no effort reads the seat; `--clear` returns it to the vendor's
/// configured effort.
fn effort(
    host: Host<'_>,
    serve: &Path,
    matches: &ArgMatches,
    contract: &Contract,
) -> Outcome<CommandAnswer> {
    let effort = match seat_edit(matches, "EFFORT") {
        Ok(SeatEdit::Read) => return seat_read(host, serve, matches, SeatSetting::Effort),
        Ok(SeatEdit::Set(effort)) => Some(effort),
        Ok(SeatEdit::Clear) => None,
        Err(outcome) => return outcome,
    };
    if effort
        .as_deref()
        .is_some_and(|effort| effort.parse::<vendor::Effort>().is_err())
    {
        return protocol_fault(
            "lane::effort-invalid",
            "effort must be low, medium, high, xhigh, or max",
        );
    }
    let (lane, topology_revision) = match retune_target(host, serve, matches) {
        Ok(target) => target,
        Err(outcome) => return outcome,
    };
    let body = LaneEffortRetune {
        topology_revision,
        lane,
        effort,
    };
    command_client::execute(
        serve,
        contract,
        SelectedCommand::LaneEffortRetune { body: &body },
        host.ambient,
    )
}

/// Turn one seated lane's own fast-mode flag on or off. The lane still runs
/// fast while its team's flag or the global setting is on.
fn fast(
    host: Host<'_>,
    serve: &Path,
    matches: &ArgMatches,
    contract: &Contract,
) -> Outcome<CommandAnswer> {
    let (lane, topology_revision) = match retune_target(host, serve, matches) {
        Ok(target) => target,
        Err(outcome) => return outcome,
    };
    let body = LaneFastRetune {
        topology_revision,
        lane,
        fast_mode: matches
            .get_one::<String>("on|off")
            .is_some_and(|mode| mode == "on"),
    };
    command_client::execute(
        serve,
        contract,
        SelectedCommand::LaneFastRetune { body: &body },
        host.ambient,
    )
}

/// Request succession for one lane, or withdraw a request that has not handed
/// off. This records intent only; the resident owns every start.
fn renew(
    host: Host<'_>,
    serve: &Path,
    matches: &ArgMatches,
    contract: &Contract,
) -> Outcome<CommandAnswer> {
    let (lane, team, topology_revision) = match renewal_target(host, serve, matches) {
        Ok(target) => target,
        Err(outcome) => return outcome,
    };
    let body = LaneRenewal {
        topology_revision,
        team,
        lane,
        requested: !matches.get_flag("clear"),
    };
    command_client::execute(
        serve,
        contract,
        SelectedCommand::LaneRenewal { body: &body },
        host.ambient,
    )
}

fn import(
    host: Host<'_>,
    serve: &Path,
    current: &Path,
    matches: &ArgMatches,
    contract: &Contract,
) -> Outcome<CommandAnswer> {
    let repository = host.repository;
    let lane = match lane_id(host, matches) {
        Ok(lane) => lane,
        Err(outcome) => return outcome,
    };
    let requested = PathBuf::from(
        matches
            .get_one::<String>("WORKTREE")
            .expect("generated syntax requires the import worktree"),
    );
    let requested = if requested.is_absolute() {
        requested
    } else {
        current.join(requested)
    };
    let target = match Repository::discover(&requested) {
        Ok(target) => target,
        Err(error) => return environment_fault("lane::worktree-unresolved", error.to_string()),
    };
    if target.common_dir() != repository.common_dir() {
        return refusal(
            "lane::foreign-worktree",
            Some(repository.common_dir().display().to_string()),
            Some(target.common_dir().display().to_string()),
            "choose an existing worktree belonging to the current Git common directory",
        );
    }
    let requested = match std::fs::canonicalize(requested) {
        Ok(requested) => requested,
        Err(error) => return environment_fault("lane::worktree-unresolved", error.to_string()),
    };
    if requested != target.worktree_root() {
        return refusal(
            "lane::worktree-not-root",
            Some(target.worktree_root().display().to_string()),
            Some(requested.display().to_string()),
            "name the root of one existing worktree",
        );
    }
    let body = LaneState {
        lane,
        action: LaneAction::Wake,
        worktree: Some(requested.display().to_string()),
    };
    command_client::execute(
        serve,
        contract,
        SelectedCommand::LaneState { body: &body },
        host.ambient,
    )
}

fn recall(
    host: Host<'_>,
    serve: &Path,
    matches: &ArgMatches,
    contract: &Contract,
) -> Outcome<CommandAnswer> {
    let lane = match matches.get_one::<String>("LANE") {
        Some(lane) => match crate::lane_selector::resolve(host, lane, "recall::lane-invalid") {
            Ok(lane) => Some(lane),
            Err(outcome) => return outcome,
        },
        None => None,
    };
    let body = LaneRecall { lane };
    command_client::execute(
        serve,
        contract,
        SelectedCommand::LaneRecall { body: &body },
        host.ambient,
    )
}

fn refusal(
    reason: &str,
    expected: Option<String>,
    observed: Option<String>,
    detail: &str,
) -> Outcome<CommandAnswer> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("worktree".to_owned()),
            expected,
            observed,
        )
        .with_detail(detail),
    ))
}

fn new_uuid() -> CanonicalUuid {
    CanonicalUuid::new(uuid::Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_add_body_does_not_read_removed_resume_argument() {
        let matches = crate::cli::command()
            .try_get_matches_from([
                "spice",
                "lane",
                "add",
                "--name",
                " reviewer ",
                "--model",
                " gpt-5.5 ",
                "--effort",
                "low",
            ])
            .expect("lane add parses without resume");
        let (_, lane_matches) = matches.subcommand().expect("lane subcommand exists");
        let (_, add_matches) = lane_matches
            .subcommand()
            .expect("lane add subcommand exists");
        let lane = CanonicalUuid::new(uuid::Uuid::from_u128(1));
        let team = CanonicalUuid::new(uuid::Uuid::from_u128(2));
        let body = lane_seat_body(
            add_matches,
            7,
            lane,
            team,
            "/tmp/spice-lane".to_owned(),
            "drive".to_owned(),
            "omp".parse().expect("fixture vendor parses"),
        );

        assert_eq!(body.topology_revision, 7);
        assert_eq!(body.lane, lane);
        assert_eq!(body.team, team);
        assert_eq!(body.name.as_deref(), Some("reviewer"));
        assert_eq!(body.worktree, "/tmp/spice-lane");
        assert_eq!(body.life, "drive");
        assert_eq!(body.vendor.as_str(), "omp");
        assert_eq!(body.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(body.effort.as_deref(), Some("low"));
        assert_eq!(body.resume, None);
    }

    #[test]
    fn lane_add_rejects_an_effort_outside_the_closed_vocabulary() {
        let error = crate::cli::command()
            .try_get_matches_from(["spice", "lane", "add", "--effort", "ultra"])
            .expect_err("an unknown effort is refused by the generated parser");
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn vendor_resolution_is_explicit_then_environment_then_configured_default() {
        let configured = "configured".parse().expect("configured vendor");
        assert_eq!(
            resolve_vendor(
                Some("explicit"),
                Some(OsString::from("environment")),
                &configured,
            )
            .expect("explicit vendor resolves")
            .as_str(),
            "explicit"
        );
        assert_eq!(
            resolve_vendor(None, Some(OsString::from(" EnViRoNmEnT ")), &configured)
                .expect("environment vendor resolves")
                .as_str(),
            "environment"
        );
        assert_eq!(
            resolve_vendor(None, Some(OsString::new()), &configured)
                .expect("configured vendor resolves")
                .as_str(),
            "configured"
        );
    }

    const LANE: &str = "00000000-0000-4000-8000-000000000001";

    fn edit(
        leaf: &str,
        value: &str,
        arguments: &[&str],
    ) -> Result<SeatEdit, Outcome<CommandAnswer>> {
        let matches = crate::cli::command()
            .try_get_matches_from(["spice", "lane", leaf, LANE].iter().chain(arguments))
            .expect("lane retune parses");
        let (_, lane) = matches.subcommand().expect("lane subcommand exists");
        let (_, leaf) = lane.subcommand().expect("retune subcommand exists");
        seat_edit(leaf, value)
    }

    #[test]
    fn omitting_the_value_reads_and_only_clear_clears() {
        for (leaf, value, named) in [
            ("model", "MODEL", "openai/gpt-5.5"),
            ("effort", "EFFORT", "high"),
        ] {
            assert_eq!(edit(leaf, value, &[]).ok(), Some(SeatEdit::Read));
            assert_eq!(
                edit(leaf, value, &[named]).ok(),
                Some(SeatEdit::Set(named.to_owned()))
            );
            assert_eq!(edit(leaf, value, &["--clear"]).ok(), Some(SeatEdit::Clear));
            assert!(matches!(
                edit(leaf, value, &[named, "--clear"]),
                Err(Outcome::Refusal(_))
            ));
        }
    }
}
