//! Daemonless RTK history resolution and query by agent identity.

use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::Command;

use clap::ArgMatches;
use wire::{
    CanonicalUuid, Fault, FaultClass, Outcome, Refusal, RefusalDiagnostic, Reply, ReplyPosition,
    RtkGainReport,
};

use spice::repository::Repository;

/// Resolve one agent database and run the configured RTK gain query against it.
pub(crate) fn gain(
    matches: &ArgMatches,
    configuration: Option<&spice::config::RepositoryCapture>,
) -> Outcome<RtkGainReport> {
    let requested = matches
        .get_one::<String>("AGENT_UUID")
        .expect("clap requires the registered agent UUID operand");
    let identity = match AgentIdentity::parse(requested) {
        Some(identity) => identity,
        None => {
            return Failure::refusal(
                "rtk::agent-invalid",
                format!(
                    "agent UUID {requested:?} is not canonical or compact lower-case UUID text"
                ),
            )
            .outcome();
        }
    };
    let repository = match current_repository() {
        Ok(repository) => repository,
        Err(failure) => return failure.outcome(),
    };
    let loaded = match spice::config::capture_repository_scoped(&repository, &[], configuration) {
        Ok(loaded) => loaded,
        Err(error) => {
            return Failure::refusal(
                "rtk::configuration-refused",
                format!("cannot resolve the configured RTK executable: {error}"),
            )
            .outcome();
        }
    };
    let database = match selected_history(&repository, &identity) {
        Ok(database) => database,
        Err(outcome) => return outcome,
    };
    let database_text = match database.to_str() {
        Some(database) => database.to_owned(),
        None => {
            return Failure::fault(
                FaultClass::Store,
                "rtk::history-path-invalid",
                format!("RTK database path is not UTF-8: {database:?}"),
            )
            .outcome();
        }
    };
    let executable = loaded.resolved().root().rtk().executable();
    let output = match Command::new(executable)
        .arg("gain")
        .current_dir(repository.worktree_root())
        .env(spice::rtk::HISTORY_VARIABLE, &database)
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            return Failure::fault(
                FaultClass::Environment,
                "rtk::command-unavailable",
                format!(
                    "configured RTK executable {executable:?} could not query {database_text}: {error}"
                ),
            )
            .outcome();
        }
    };
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        return Failure::fault(
            FaultClass::Environment,
            "rtk::command-failed",
            format!(
                "configured RTK executable {executable:?} failed for {database_text} with {}; stderr: {}",
                output.status,
                diagnostic.trim_end()
            ),
        )
        .outcome();
    }
    let output = match String::from_utf8(output.stdout) {
        Ok(output) => output,
        Err(error) => {
            return Failure::fault(
                FaultClass::Protocol,
                "rtk::output-invalid",
                format!("rtk gain output for {database_text} is not UTF-8: {error}"),
            )
            .outcome();
        }
    };

    Outcome::Reply(Reply {
        position: ReplyPosition::Direct {},
        body: RtkGainReport {
            agent: requested.clone(),
            database: database_text,
            output,
        },
    })
}

fn current_repository() -> Result<Repository, Failure> {
    let current = std::env::current_dir().map_err(|error| {
        Failure::fault(
            FaultClass::Environment,
            "rtk::working-directory-unavailable",
            error.to_string(),
        )
    })?;
    Repository::discover(&current).map_err(|error| {
        Failure::fault(
            FaultClass::Environment,
            spice::repository::ResolveError::REASON,
            error.to_string(),
        )
    })
}

fn selected_history(
    repository: &Repository,
    identity: &AgentIdentity,
) -> Result<PathBuf, Outcome<RtkGainReport>> {
    let state = repository
        .admit_state()
        .map_err(|error| crate::state_failure(&error).outcome())?;
    let current = state.rtk_history(identity.uuid);
    resolve_history(current, &identity.canonical).map_err(Failure::outcome)
}

#[derive(Debug)]
struct AgentIdentity {
    uuid: CanonicalUuid,
    canonical: String,
}

impl AgentIdentity {
    fn parse(text: &str) -> Option<Self> {
        let uuid = text.parse::<CanonicalUuid>().ok().or_else(|| {
            (text.len() == 32
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
            .then(|| uuid::Uuid::parse_str(text).ok().map(CanonicalUuid::new))
            .flatten()
        })?;
        Some(Self {
            canonical: uuid.to_string(),
            uuid,
        })
    }
}

#[derive(Debug)]
struct HistoryObservation {
    path: PathBuf,
    problem: Option<String>,
}

impl HistoryObservation {
    fn describe(&self) -> String {
        self.problem.as_ref().map_or_else(
            || format!("{} (readable)", self.path.display()),
            |problem| format!("{} ({problem})", self.path.display()),
        )
    }
}

fn resolve_history(current: PathBuf, canonical: &str) -> Result<PathBuf, Failure> {
    if let Some(observed) = observe_history(current.clone()) {
        return match observed.problem {
            None => Ok(observed.path),
            Some(_) => Err(Failure::fault(
                FaultClass::Store,
                "rtk::history-unreadable",
                format!("agent history is unavailable at {}", observed.describe()),
            )),
        };
    }

    Err(Failure::refusal(
        "rtk::history-absent",
        format!(
            "no current Rust RTK history database was found for agent {} at {}",
            canonical,
            current.display()
        ),
    ))
}

fn observe_history(path: PathBuf) -> Option<HistoryObservation> {
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return None,
        Err(error) => {
            return Some(HistoryObservation {
                path,
                problem: Some(format!("metadata unavailable: {error}")),
            });
        }
    };
    let problem = if metadata.file_type().is_symlink() {
        Some("symbolic links are not admitted".to_owned())
    } else if !metadata.is_file() {
        Some("not a regular file".to_owned())
    } else {
        File::open(&path)
            .err()
            .map(|error| format!("unreadable: {error}"))
    };
    Some(HistoryObservation { path, problem })
}

enum Failure {
    Refusal(Refusal),
    Fault(Fault),
}

impl Failure {
    fn refusal(reason: &str, detail: String) -> Self {
        Self::Refusal(Refusal::bare(
            RefusalDiagnostic::new(reason.to_owned(), Some("agent-uuid".to_owned()), None, None)
                .with_detail(detail),
        ))
    }

    fn fault(class: FaultClass, reason: &str, detail: String) -> Self {
        Self::Fault(Fault {
            class,
            reason: reason.to_owned(),
            detail,
        })
    }

    fn outcome(self) -> Outcome<RtkGainReport> {
        match self {
            Self::Refusal(refusal) => Outcome::Refusal(refusal),
            Self::Fault(fault) => Outcome::Fault(fault),
        }
    }
}
