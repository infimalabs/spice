//! The record a resident writes into the lock file it possesses.
//!
//! The kernel's answer about the lock says whether some resident is running.
//! It cannot say which one, and that distance is the whole difference between
//! dialling a live surface and dialling an address nobody serves: a resident
//! holds this lock from its first instant, while the discovery records the
//! previous resident published are still on disk and still name the previous
//! resident's generation.
//!
//! So a holder writes down which generation it is, and readers compare. The
//! grammar is stated once here because its two halves live far apart -- the
//! resident writes it from `up`, and clients parse it across a process
//! boundary and an integration-test boundary besides. A reader that restated
//! the grammar and then drifted from the writer would not fail loudly: it
//! would report every live resident as a stranger, and each refusal it raised
//! would look exactly like the defect this record exists to catch.

use std::fmt;
use std::net::SocketAddr;

use wire::CanonicalUuid;

/// What a holder names before it has a generation to name.
///
/// The lock is taken before the epoch is minted, so the first record a
/// resident writes cannot carry one. The spelling is deliberately not a UUID:
/// no published discovery can ever carry it, so a reader comparing generations
/// reads the whole startup interval as the mismatch it is rather than as a
/// record it may trust.
const PENDING_EPOCH: &str = "pending";
/// Smallest bearer token the resident accepts.
pub const AUTH_TOKEN_MIN_BYTES: usize = 16;
/// Largest bearer token the resident accepts.
pub const AUTH_TOKEN_MAX_BYTES: usize = 256;

/// Access policy a running resident must preserve across a developer refresh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResidentAccess {
    /// Only callers originating on this machine are admitted.
    Local,
    /// The operator explicitly consented to unauthenticated network access.
    Insecure,
    /// Every caller must present the operator's bearer token.
    Authenticated,
}

impl ResidentAccess {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Insecure => "insecure",
            Self::Authenticated => "authenticated",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "local" => Some(Self::Local),
            "insecure" => Some(Self::Insecure),
            "authenticated" => Some(Self::Authenticated),
            _ => None,
        }
    }
}

/// Exact live bind and access policy needed to restart one resident.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResidentLaunch {
    listen: SocketAddr,
    access: ResidentAccess,
}

impl ResidentLaunch {
    /// Build restart metadata after the kernel has resolved an ephemeral port.
    #[must_use]
    pub const fn new(listen: SocketAddr, access: ResidentAccess) -> Self {
        Self { listen, access }
    }

    /// Address the resident actually bound.
    #[must_use]
    pub const fn listen(self) -> SocketAddr {
        self.listen
    }

    /// Access policy the resident applies at that address.
    #[must_use]
    pub const fn access(self) -> ResidentAccess {
        self.access
    }
}

/// A holder carried launch metadata but it was not one complete supported value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaunchRecordError;

impl fmt::Display for LaunchRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("resident holder launch metadata is malformed")
    }
}

impl std::error::Error for LaunchRecordError {}

/// The record a resident publishes while it possesses the lock.
pub fn holder_record(pid: u32, epoch: Option<CanonicalUuid>) -> String {
    match epoch {
        Some(epoch) => format!("pid={pid};epoch={epoch}"),
        None => format!("pid={pid};epoch={PENDING_EPOCH}"),
    }
}
/// The record a resident publishes once its listening socket is bound.
#[must_use]
pub fn launched_holder_record(pid: u32, epoch: CanonicalUuid, launch: ResidentLaunch) -> String {
    format!(
        "{};launch={},{}",
        holder_record(pid, Some(epoch)),
        launch.access.as_str(),
        launch.listen
    )
}

/// Launch metadata a complete holder record carries.
///
/// A holder that has not yet bound its listening socket publishes no `launch`
/// field and answers `Ok(None)`. A present field is strict: silently ignoring
/// damaged metadata could restart a wildcard listener on loopback or drop
/// authentication.
pub fn holder_launch(record: &[u8]) -> Result<Option<ResidentLaunch>, LaunchRecordError> {
    let line = std::str::from_utf8(record)
        .map_err(|_| LaunchRecordError)?
        .lines()
        .next()
        .ok_or(LaunchRecordError)?;
    let mut launch = line
        .split(';')
        .filter_map(|field| field.trim().strip_prefix("launch="));
    let Some(value) = launch.next() else {
        return Ok(None);
    };
    if launch.next().is_some() {
        return Err(LaunchRecordError);
    }
    let (access, listen) = value.split_once(',').ok_or(LaunchRecordError)?;
    let access = ResidentAccess::parse(access).ok_or(LaunchRecordError)?;
    let listen = listen
        .parse::<SocketAddr>()
        .map_err(|_| LaunchRecordError)?;
    let policy_mismatches_address = match access {
        ResidentAccess::Local => !listen.ip().is_loopback(),
        ResidentAccess::Insecure => listen.ip().is_loopback(),
        ResidentAccess::Authenticated => false,
    };
    if listen.port() == 0 || policy_mismatches_address {
        return Err(LaunchRecordError);
    }
    Ok(Some(ResidentLaunch { listen, access }))
}

/// Whether a bearer token satisfies the resident's public token contract.
#[must_use]
pub fn auth_token_is_valid(token: &str) -> bool {
    (AUTH_TOKEN_MIN_BYTES..=AUTH_TOKEN_MAX_BYTES).contains(&token.len())
        && token.bytes().all(is_token_byte)
}

const fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// The generation a holder record names, if it names a complete one.
///
/// Every other shape answers `None`, and every caller reads that answer the
/// same way: whoever holds this lock did not publish the record you are
/// holding. An empty record is a holder on its way out, which truncates before
/// it releases; a pending record is a holder that has not minted its
/// generation yet; anything unparsable is a holder this reader cannot vouch
/// for. None of the three is the publisher, so none of them earns a dial.
///
/// Only the first line is read. A holder rewrite pads its record out to the
/// span it replaces so no reader ever lands in an empty file, and that padding
/// is trailing filler, not a second field.
pub fn holder_epoch(record: &[u8]) -> Option<CanonicalUuid> {
    std::str::from_utf8(record)
        .ok()?
        .lines()
        .next()?
        .split(';')
        .find_map(|field| field.trim().strip_prefix("epoch="))?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use sill::{Acquisition, LockFile, LockName};

    use super::*;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn uuid(last: u8) -> CanonicalUuid {
        format!("00000000-0000-4000-8000-{last:012x}")
            .parse()
            .expect("canonical fixture UUID")
    }

    /// A record survives the framing the lock actually writes it through.
    ///
    /// The reader is paired against `sill`'s own rewrite rather than against a
    /// restated byte layout, because the layout is not this module's to state:
    /// the newline and the padding that keeps a contended reader out of an
    /// empty file are added underneath. A test that rebuilt them here would go
    /// on passing after `sill` changed how a record is landed, which is the
    /// one drift that leaves both halves of this grammar looking correct.
    ///
    /// The long record is written first on purpose. The short one that follows
    /// is the padded case, and a reader that stopped at the first newline is
    /// the only one that survives both.
    #[test]
    fn a_holder_record_round_trips_through_the_lock_that_frames_it() {
        let path = std::env::temp_dir().join(format!(
            "spice-holder-record-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let lock = LockFile::new(
            LockName::new("resident").expect("resident is a non-empty lock name"),
            &path,
        );
        let Acquisition::Acquired(possession) = lock.try_acquire().expect("fixture lock") else {
            panic!("a lock file nothing else has seen cannot be contended");
        };

        let launch = ResidentLaunch::new(
            "[::]:8767".parse().expect("fixture address"),
            ResidentAccess::Insecure,
        );
        possession
            .rewrite_record(launched_holder_record(4321, uuid(1), launch).as_bytes())
            .expect("long holder record writes");
        let published = std::fs::read(&path).expect("long record reads");
        assert_eq!(holder_epoch(&published), Some(uuid(1)));
        assert_eq!(holder_launch(&published), Ok(Some(launch)));

        possession
            .rewrite_record(holder_record(7, None).as_bytes())
            .expect("padded holder record writes");
        let padded = std::fs::read(&path).expect("padded record reads");
        assert!(
            padded.len() > holder_record(7, None).len() + 1,
            "the shorter record must land padded, or this case is the long one again"
        );
        assert_eq!(
            holder_epoch(&padded),
            None,
            "a holder that has not minted its generation names none"
        );
        assert_eq!(
            holder_launch(&padded),
            Ok(None),
            "a pending holder must not retain the previous generation's launch"
        );

        drop(possession);
        std::fs::remove_file(&path).expect("fixture cleanup");
    }

    #[test]
    fn launch_metadata_round_trips_without_exposing_an_authentication_secret() {
        let launch = ResidentLaunch::new(
            "0.0.0.0:8767".parse().expect("fixture address"),
            ResidentAccess::Authenticated,
        );
        let record = launched_holder_record(7, uuid(3), launch);

        assert_eq!(holder_epoch(record.as_bytes()), Some(uuid(3)));
        assert_eq!(holder_launch(record.as_bytes()), Ok(Some(launch)));
        assert!(!record.contains("token"));
        assert_eq!(
            holder_launch(format!("pid=7;epoch={};launch=local,0.0.0.0:8767", uuid(3)).as_bytes()),
            Err(LaunchRecordError),
            "a local policy cannot describe a remotely reachable bind"
        );
    }

    /// Nothing but a complete generation is read as one.
    ///
    /// The truncated record is what a holder leaves behind while it releases,
    /// and the pid-only record is what a future field order could look like
    /// from here. Both are refusals rather than trust, and the last case pins
    /// that the field is matched whole: a longer name ending in `epoch` is a
    /// different field and must not be mistaken for this one.
    #[test]
    fn no_partial_holder_record_names_a_generation() {
        assert_eq!(holder_epoch(b""), None);
        assert_eq!(holder_epoch(b"\n"), None);
        assert_eq!(holder_epoch(b"pid=7\n"), None);
        assert_eq!(holder_epoch(b"pid=7;epoch=\n"), None);
        assert_eq!(holder_epoch(b"pid=7;epoch=not-a-generation\n"), None);
        assert_eq!(holder_epoch(&[0xff, 0xfe]), None);
        assert_eq!(
            holder_epoch(format!("pid=7;prior-epoch={}\n", uuid(2)).as_bytes()),
            None,
            "a field whose name merely ends in the one under test is not it"
        );
        assert_eq!(
            holder_epoch(format!("pid=7;epoch={}\n", uuid(2)).as_bytes()),
            Some(uuid(2)),
            "the negative cases are only meaningful against a reader that still reads"
        );
    }
}
