//! The `spice://` server a lane's host frame reads through, and `spice
//! resource read`, which prints exactly what that frame would carry.

use std::path::PathBuf;

use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::intent::TextAnswer;
use wire::{CanonicalUuid, Outcome, Refusal, Reply, ReplyPosition};

use super::{Access, Seat};

/// Answers one `spice://` read or write with its result, or the refusal.
pub(crate) type Server = Box<dyn Fn(&str, Access<'_>) -> Result<String, String> + Send + Sync>;

/// Serve `spice://` URLs as `lane`, whose checkout is `worktree` and whose
/// occupant's environment is `ambient`, resolving quote targets, CLI reads,
/// and task text writes through the resident. A human-authored quote names
/// its author from the principal index of the journal at `journal`.
pub(crate) fn lane_server(
    discovery: PathBuf,
    journal: PathBuf,
    lane: CanonicalUuid,
    worktree: PathBuf,
    ambient: Ambient,
) -> Server {
    let seat = Seat {
        worktree,
        lane,
        environment: crate::session_environment(&ambient),
        ambient,
        graphs: Default::default(),
    };
    Box::new(move |url, access| {
        seat.serve(
            url,
            access,
            |target| match crate::command_client::read_lane_quote(
                &discovery,
                lane,
                target,
                &seat.ambient,
            ) {
                Ok(resolution) => {
                    let principals = spice::quote::author_names(&journal, &resolution)?;
                    spice::quote::render(&resolution, principals.as_ref())
                }
                Err(Outcome::Refusal(refusal)) => Err(format!(
                    "spice refused the quote read: {}",
                    refusal.reason()
                )),
                Err(Outcome::Fault(fault)) => Err(format!(
                    "spice could not read the quote: {}: {}",
                    fault.reason, fault.detail
                )),
                Err(Outcome::Reply(_)) => {
                    Err("spice answered the quote read with no resolution".to_owned())
                }
            },
        )
    })
}

/// `spice resource read URL`: the text this lane's host frame carries for
/// `URL`, read through the same server, or its refusal.
pub(crate) fn read_command(leaf: &clap::ArgMatches, ambient: &Ambient) -> Outcome<TextAnswer> {
    let url = leaf
        .get_one::<String>("URL")
        .expect("the generated parser requires URL");
    let Some(lane) = ambient
        .get(steer::hook::LANE)
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse::<CanonicalUuid>().ok())
    else {
        return Outcome::Refusal(refusal(
            "resource::lane-unset",
            None,
            "run spice resource read in a lane's shell, which names the lane it reads as",
        ));
    };
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(error) => return environment_fault("resource::cwd-unavailable", &error),
    };
    let repository = match Repository::discover(&current) {
        Ok(repository) => repository,
        Err(error) => {
            return environment_fault(spice::repository::ResolveError::REASON, &error);
        }
    };
    let state = match repository.admit_state() {
        Ok(state) => state,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let server = lane_server(
        state.serve().to_path_buf(),
        state.journal().to_path_buf(),
        lane,
        repository.worktree_root().to_path_buf(),
        ambient.clone(),
    );
    match server(url, Access::Read) {
        Ok(text) => Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body: TextAnswer { text },
        }),
        Err(reason) => Outcome::Refusal(refusal("resource::refused", Some(url.clone()), &reason)),
    }
}

fn refusal(reason: &str, observed: Option<String>, detail: &str) -> Refusal {
    Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("URL".to_owned()),
            Some("one spice:// URL this lane serves".to_owned()),
            observed,
        )
        .with_detail(detail),
    )
}

fn environment_fault(reason: &str, error: &dyn std::fmt::Display) -> Outcome<TextAnswer> {
    crate::environment_fault(reason, error.to_string())
}
