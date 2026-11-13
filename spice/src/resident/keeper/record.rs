//! The owner-only record of one installed lifecycle owner.
//!
//! The record carries everything the keeper needs to relaunch the resident
//! exactly as it was installed, including the bearer token, which is why it
//! is a private file in the repository's runtime root and never an argument
//! of the registered task.

use std::fs;
use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sill::authority::create_private_file;

/// Scratch name the record is written to before replacing the current one.
const REPLACEMENT_SUFFIX: &str = ".next";

/// One installed lifecycle owner.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KeeperRecord {
    /// Stable identity of the native per-user registration.
    #[serde(rename = "task")]
    pub(crate) registration: String,
    /// The appliance registered as the native manager's action.
    pub(crate) executable: PathBuf,
    /// Digest of the appliance bytes the running keeper must launch.
    pub(crate) binary: wire::Digest,
    /// Directory in which the native manager starts the keeper.
    pub(crate) working_directory: PathBuf,
    /// Address the kept resident listens on; port 0 until the first resident
    /// bound one, which is then pinned so relaunches keep the endpoint.
    pub(crate) listen: SocketAddr,
    /// Whether a non-loopback bind was consented to at install.
    pub(crate) allow_insecure_bind: bool,
    /// Bearer token every kept resident requires, handed over privately.
    pub(crate) auth_token: Option<String>,
    /// Whether package application created the loopback-default policy.
    ///
    /// Its system-selected pinned port may move from zero to a concrete port
    /// without changing the digest-bound semantic standing.
    #[serde(default)]
    pub(crate) managed_default: bool,
}

impl KeeperRecord {
    /// The argument vector the keeper launches the resident with; the token
    /// travels over the launch handoff instead.
    pub(crate) fn launch_argv(&self) -> Vec<String> {
        let mut argv = vec![
            "spice".to_owned(),
            "--json".to_owned(),
            "up".to_owned(),
            "--host".to_owned(),
            self.listen.ip().to_string(),
            "--port".to_owned(),
            self.listen.port().to_string(),
        ];
        if self.allow_insecure_bind {
            argv.push("--allow-insecure-bind".to_owned());
        }
        argv
    }

    /// Pin the port a resident actually bound when the install left it to
    /// the system; a record that already names a port is unchanged.
    pub(crate) fn pin_port(&mut self, endpoint: &str) -> bool {
        if self.listen.port() != 0 {
            return false;
        }
        let Some(port) = endpoint_port(endpoint) else {
            return false;
        };
        self.listen.set_port(port);
        true
    }

    /// Keep the port a previous install pinned when this one left the port
    /// to the system on the same host, so an unchanged install stays put.
    pub(crate) fn inherit_port(&mut self, previous: &Self) {
        if self.listen.port() == 0 && self.listen.ip() == previous.listen.ip() {
            self.listen.set_port(previous.listen.port());
        }
    }

    /// Whether applying `next` requires replacing the running native owner
    /// rather than merely letting the current keeper relaunch its child.
    pub(crate) fn owner_changed_by(&self, next: &Self) -> bool {
        self.registration != next.registration
            || self.binary != next.binary
            || self.executable != next.executable
            || self.working_directory != next.working_directory
    }

    /// Whether applying `next` must retire the current resident or owner.
    ///
    /// The running keeper launches its own executable, so changing the native
    /// registration, appliance bytes, executable path, or working directory is
    /// as material as changing the resident's listen or access policy.
    pub(crate) fn reconfigured_by(&self, next: &Self) -> bool {
        self.owner_changed_by(next)
            || self.listen != next.listen
            || self.allow_insecure_bind != next.allow_insecure_bind
            || self.auth_token != next.auth_token
    }
    /// Digest-bound policy standing exposed to install planning without
    /// publishing the bearer token.
    pub(crate) fn standing(&self) -> io::Result<spice::install::LifecycleOwnerStanding> {
        if self.managed_default {
            return Ok(spice::install::LifecycleOwnerStanding::Default);
        }
        #[derive(Serialize)]
        struct Policy<'a> {
            listen: SocketAddr,
            allow_insecure_bind: bool,
            auth_token: &'a Option<String>,
        }
        let policy = Policy {
            listen: self.listen,
            allow_insecure_bind: self.allow_insecure_bind,
            auth_token: &self.auth_token,
        };
        let mut bytes = b"spice keeper policy standing v1\0".to_vec();
        bytes.extend(
            serde_json::to_vec(&policy)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        );
        Ok(spice::install::LifecycleOwnerStanding::Configured(
            wire::Digest::compute(wire::Domain::InstallPlan, &bytes),
        ))
    }
}

/// The port of one `scheme://host:port` endpoint.
pub(crate) fn endpoint_port(endpoint: &str) -> Option<u16> {
    let authority = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest);
    let authority = authority.split('/').next()?;
    let (_, port) = authority.rsplit_once(':')?;
    port.parse().ok()
}

/// Read the record at `path`; `None` when no owner is installed.
///
/// # Errors
///
/// Returns the read failure or the malformed record.
pub(crate) fn read(path: &Path) -> io::Result<Option<KeeperRecord>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Write `record` to `path` privately and atomically.
///
/// # Errors
///
/// Returns the failure creating, writing, or renaming the private file.
pub(crate) fn write(path: &Path, record: &KeeperRecord) -> io::Result<()> {
    let encoded = serde_json::to_vec_pretty(record)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    replace_private(path, &encoded)
}

/// Replace `path` with an owner-only file holding `contents`, atomically.
///
/// # Errors
///
/// Returns the failure creating, writing, or renaming the private file.
pub(crate) fn replace_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut scratch = path.as_os_str().to_owned();
    scratch.push(REPLACEMENT_SUFFIX);
    let scratch = PathBuf::from(scratch);
    match fs::remove_file(&scratch) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut file = create_private_file(&scratch)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&scratch, path)
}
