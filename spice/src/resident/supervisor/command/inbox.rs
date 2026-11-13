//! The lane's rendered steering, read through the installed executable.
//!
//! A supervisor outlives resident restarts, and a restart may move the
//! resident to a release whose command protocol this supervisor's build cannot
//! open a session with. The installed executable always speaks the resident's
//! release, so every inbox read runs it under [`ENTRY`]: it reads the lane's
//! authenticated inbox, projects pending media for the exact occupant its
//! projection state path names, and prints the rendered steering. The entry's
//! arguments and the projection state file are the contract between releases.

use std::ffi::OsString;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::{Mutex, PoisonError};

use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::CanonicalUuid;

use super::super::super::RuntimePaths;

mod media;

/// The hidden entry an inbox read runs under.
pub(in crate::resident::supervisor) const ENTRY: &str = "__lane-inbox";

/// What the occupant was shown of its steering media, beside its state.
const PROJECTION_STATE: &str = "steering-media.json";

/// One occupant's way to its current rendered inbox.
pub(in crate::resident::supervisor) struct Reader {
    executable: PathBuf,
    worktree: PathBuf,
    lane: CanonicalUuid,
    projection: PathBuf,
    /// One read at a time, since each carries the projection state forward.
    serial: Mutex<()>,
}

impl Reader {
    /// Read through this process's executable path, which the installer
    /// replaces in place, from `worktree`, carrying what the occupant whose
    /// supervisor state is `state` was shown beside that state.
    ///
    /// # Errors
    ///
    /// Returns the error naming why this executable's path is unknown.
    pub(super) fn new(state: &Path, lane: CanonicalUuid, worktree: &Path) -> io::Result<Self> {
        Ok(Self {
            executable: std::env::current_exe()?,
            worktree: worktree.to_path_buf(),
            lane,
            projection: state.with_file_name(PROJECTION_STATE),
            serial: Mutex::new(()),
        })
    }

    /// The rendered pending steering, or `None` when the resident could not
    /// answer; the entry's own diagnostics reach this supervisor's log.
    pub(super) fn read(&self) -> Option<Vec<u8>> {
        let _serial = self.serial.lock().unwrap_or_else(PoisonError::into_inner);
        let output = ProcessCommand::new(&self.executable)
            .arg(ENTRY)
            .arg(self.lane.to_string())
            .arg(&self.projection)
            .current_dir(&self.worktree)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output();
        match output {
            Ok(output) if output.status.success() => Some(output.stdout),
            Ok(output) => {
                eprintln!("spice supervisor: inbox read ended {}", output.status);
                None
            }
            Err(error) => {
                eprintln!("spice supervisor: inbox read did not start: {error}");
                None
            }
        }
    }
}

/// Print `lane`'s rendered pending steering for the supervisor that ran this
/// entry from the lane's worktree, on behalf of the exact occupant whose
/// projection state is `projection`.
///
/// A state path that names no occupant of `lane`, or an occupant that no
/// longer holds the lane, refuses before any media is touched.
pub(in crate::resident::supervisor) fn run(
    lane: &OsString,
    projection: &Path,
    ambient: &Ambient,
) -> io::Result<()> {
    let lane = lane
        .to_str()
        .and_then(|lane| lane.parse::<CanonicalUuid>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "lane is not a UUID"))?;
    let worktree = std::env::current_dir()?;
    let repository = Repository::discover(&worktree).map_err(io::Error::other)?;
    let paths = RuntimePaths::from_repository(&repository)
        .map_err(|outcome| io::Error::other(format!("{outcome:?}")))?;
    let occupant = media::occupant(&paths.supervisors, lane, projection, PROJECTION_STATE)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let inbox = crate::command_client::read_lane_inbox(&paths.serve, lane, ambient)
        .map_err(|outcome| io::Error::other(format!("{outcome:?}")))?;
    let pending = inbox
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .ok_or_else(|| io::Error::other("the resident's inbox answer omits this lane"))?;
    // Direction outranks its pictures: a projection failure still delivers
    // the text and references, and says why on stderr.
    let projected = match media::project(&paths.steering_media, lane, occupant, projection, pending)
    {
        Ok(projected) => projected,
        Err(media::Refusal::Stale(error)) => return Err(io::Error::other(error)),
        Err(media::Refusal::Failed(error)) => {
            eprintln!("spice supervisor: steering media projection failed: {error}");
            media::Projected::new()
        }
    };
    let now = wire::Instant::new(jiff::Timestamp::now()).map_err(io::Error::other)?;
    let rendered = steer::inbox::render_pending(
        &steer::Key::for_identity(lane),
        &pending.pending,
        now,
        |directive, ordinal| projected.get(&(directive, ordinal)).cloned(),
    );
    let mut stdout = io::stdout().lock();
    stdout.write_all(rendered.as_bytes())?;
    stdout.flush()
}
