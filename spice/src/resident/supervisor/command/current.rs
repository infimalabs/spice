//! Supervisor reads whose implementation must follow the installed release.
//!
//! A lane supervisor deliberately survives resident replacement. Its executable
//! path is stable while installation replaces the image at that path, so a
//! one-shot session read and a stateful resource helper opened from the path
//! execute the resident's current protocol and journal schema rather than the
//! supervisor's retained one.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;

use spice::ambient::Ambient;
use wire::CanonicalUuid;

use super::Terminal;

mod resource;

/// Hidden entry for one current-release briefing or resume notice.
pub(in crate::resident::supervisor) const SESSION_ENTRY: &str = "__lane-session-read";

const BRIEFING: &str = "briefing";
const RESUME: &str = "resume";
const UNREAD_RESUME: &str = "<system-notice>\n# Seat resumed\n\nThis seat resumes its previous session with its context intact, so no briefing page is attached; continue from your context.\n- held claim: unread; `spice session briefing` reads it\n- pending steering keys: unread; `spice session briefing` reads it\n</system-notice>";

/// One retained supervisor's route to session reads in the executable image
/// currently published at its own path.
pub(super) struct Sessions {
    executable: PathBuf,
    worktree: PathBuf,
    lane: CanonicalUuid,
}

impl Sessions {
    pub(super) fn new(lane: CanonicalUuid, worktree: PathBuf) -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            executable: std::env::current_exe()?,
            worktree,
            lane,
        }))
    }

    pub(super) fn briefing(&self) -> Option<String> {
        match self.read(BRIEFING) {
            Ok(briefing) => Some(briefing),
            Err(error) => {
                eprintln!("spice supervisor: current briefing read failed: {error}");
                None
            }
        }
    }

    pub(super) fn resume(&self) -> String {
        self.read(RESUME).unwrap_or_else(|error| {
            eprintln!("spice supervisor: current resume read failed: {error}");
            UNREAD_RESUME.to_owned()
        })
    }

    fn read(&self, operation: &str) -> io::Result<String> {
        let output = ProcessCommand::new(&self.executable)
            .arg(SESSION_ENTRY)
            .arg(operation)
            .arg(self.lane.to_string())
            .current_dir(&self.worktree)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "session reader ended {}",
                output.status
            )));
        }
        String::from_utf8(output.stdout)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

/// One stateful current-release resource server for this occupant.
pub(super) fn resource_server(
    lane: CanonicalUuid,
    worktree: PathBuf,
    terminal: Option<Terminal>,
) -> io::Result<crate::resource::Server> {
    let reader = resource::Reader::new(lane, worktree, terminal)?;
    Ok(Box::new(move |url, access| reader.serve(url, access)))
}

/// Dispatch a hidden current-release read before normal CLI parsing.
pub(in crate::resident::supervisor) fn entry(
    role: &OsStr,
    arguments: &mut impl Iterator<Item = OsString>,
    ambient: &Ambient,
) -> Option<i32> {
    if role == OsStr::new(SESSION_ENTRY) {
        let (Some(operation), Some(lane), None) =
            (arguments.next(), arguments.next(), arguments.next())
        else {
            eprintln!("lane session read takes an operation and a lane");
            return Some(2);
        };
        return Some(match run_session(&operation, &lane, ambient) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("lane session read failed: {error}");
                1
            }
        });
    }
    if role == OsStr::new(resource::ENTRY) {
        let (Some(lane), Some(worktree), None) =
            (arguments.next(), arguments.next(), arguments.next())
        else {
            eprintln!("lane resource reader takes a lane and a worktree");
            return Some(2);
        };
        return Some(match resource::run(&lane, &worktree, ambient) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("lane resource reader failed: {error}");
                1
            }
        });
    }
    None
}

fn run_session(operation: &OsString, lane: &OsString, ambient: &Ambient) -> io::Result<()> {
    let operation = operation
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "operation is not UTF-8"))?;
    let lane = lane
        .to_str()
        .and_then(|lane| lane.parse::<CanonicalUuid>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "lane is not a UUID"))?;
    let worktree = std::env::current_dir()?;
    let environment = crate::session_environment(ambient).rooted(worktree);
    let rendered = match operation {
        BRIEFING => crate::session_command::lane_briefing(&environment, lane),
        RESUME => crate::session_command::lane_resume_notice(&environment, lane),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown session read {operation:?}"),
            ));
        }
    };
    let mut stdout = io::stdout().lock();
    stdout.write_all(rendered.as_bytes())?;
    stdout.flush()
}
