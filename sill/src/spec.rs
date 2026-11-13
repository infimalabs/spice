#[cfg(any(unix, windows))]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(any(unix, windows))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(unix, windows))]
use std::sync::{Mutex, Weak};
use std::time::Duration;

#[cfg(any(unix, windows))]
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Argv {
    program: OsString,
    arguments: Vec<OsString>,
}

impl Argv {
    pub fn new<I, S>(program: impl Into<OsString>, arguments: I) -> Result<Self, SpecError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let program = program.into();
        if program.is_empty() {
            return Err(SpecError::EmptyProgram);
        }

        Ok(Self {
            program,
            arguments: arguments.into_iter().map(Into::into).collect(),
        })
    }

    pub fn program(&self) -> &OsStr {
        &self.program
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Environment(Vec<(OsString, OsString)>);

impl Environment {
    pub fn empty() -> Self {
        Self(Vec::new())
    }

    pub fn new<I, K, V>(entries: I) -> Result<Self, SpecError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        let entries = entries
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect::<Vec<_>>();
        let mut keys = BTreeSet::new();

        for (key, _) in &entries {
            if key.is_empty() {
                return Err(SpecError::EmptyEnvironmentKey);
            }
            if !keys.insert(key.clone()) {
                return Err(SpecError::DuplicateEnvironmentKey(key.clone()));
            }
        }

        Ok(Self(entries))
    }

    pub fn entries(&self) -> &[(OsString, OsString)] {
        &self.0
    }
}

/// An environment on record is its entries in order, each key and value the
/// text itself, so the record reads as the shell would show it and parses at
/// the speed of a string; a value that is not Unicode is kept as its native
/// units: bytes on Unix, UTF-16 code units on Windows.
#[cfg(any(unix, windows))]
impl Serialize for Environment {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0
            .iter()
            .map(|(key, value)| (Text::from(key.as_os_str()), Text::from(value.as_os_str())))
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
}

#[cfg(any(unix, windows))]
impl<'de> Deserialize<'de> for Environment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let entries = Vec::<(Text, Text)>::deserialize(deserializer)?;
        Self::new(
            entries
                .into_iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value))),
        )
        .map_err(serde::de::Error::custom)
    }
}

/// One environment key or value on record.
#[cfg(any(unix, windows))]
#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum Text {
    Utf8(String),
    #[cfg(unix)]
    Bytes {
        bytes: Vec<u8>,
    },
    #[cfg(windows)]
    Wide {
        wide: Vec<u16>,
    },
}

#[cfg(unix)]
impl From<&OsStr> for Text {
    fn from(text: &OsStr) -> Self {
        use std::os::unix::ffi::OsStrExt;
        text.to_str().map_or_else(
            || Self::Bytes {
                bytes: text.as_bytes().to_vec(),
            },
            |utf8| Self::Utf8(utf8.to_owned()),
        )
    }
}

#[cfg(windows)]
impl From<&OsStr> for Text {
    fn from(text: &OsStr) -> Self {
        use std::os::windows::ffi::OsStrExt;
        text.to_str().map_or_else(
            || Self::Wide {
                wide: text.encode_wide().collect(),
            },
            |utf8| Self::Utf8(utf8.to_owned()),
        )
    }
}

#[cfg(unix)]
impl From<Text> for OsString {
    fn from(text: Text) -> Self {
        use std::os::unix::ffi::OsStringExt;
        match text {
            Text::Utf8(utf8) => Self::from(utf8),
            Text::Bytes { bytes } => Self::from_vec(bytes),
        }
    }
}

#[cfg(windows)]
impl From<Text> for OsString {
    fn from(text: Text) -> Self {
        use std::os::windows::ffi::OsStringExt;
        match text {
            Text::Utf8(utf8) => Self::from(utf8),
            Text::Wide { wide } => Self::from_wide(&wide),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessGroup {
    Inherit,
    New,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Deadlines {
    runtime: Duration,
    output_drain: Duration,
    reap: Duration,
}

impl Deadlines {
    pub fn new(
        runtime: Duration,
        output_drain: Duration,
        reap: Duration,
    ) -> Result<Self, SpecError> {
        if output_drain.is_zero() {
            return Err(SpecError::ZeroOutputDrainDeadline);
        }
        if reap.is_zero() {
            return Err(SpecError::ZeroReapDeadline);
        }

        Ok(Self {
            runtime,
            output_drain,
            reap,
        })
    }

    pub fn runtime(self) -> Duration {
        self.runtime
    }

    pub fn output_drain(self) -> Duration {
        self.output_drain
    }

    pub fn reap(self) -> Duration {
        self.reap
    }
}

#[derive(Clone, Debug)]
pub enum Cancellation {
    Never,
    Token(CancellationToken),
}

impl Cancellation {
    #[cfg(any(unix, windows))]
    pub(crate) fn is_cancelled(&self) -> bool {
        match self {
            Self::Never => false,
            Self::Token(token) => token.is_cancelled(),
        }
    }

    #[cfg(any(unix, windows))]
    pub(crate) fn register_waker(
        &self,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<CancellationRegistration> {
        match self {
            Self::Never => None,
            Self::Token(token) => token.register_waker(waker),
        }
    }
}

#[derive(Clone)]
pub struct CancellationToken(Arc<CancellationState>);

struct CancellationState {
    cancelled: AtomicBool,
    #[cfg(any(unix, windows))]
    next_waker: AtomicU64,
    #[cfg(any(unix, windows))]
    wakers: Mutex<BTreeMap<u64, Arc<dyn Fn() + Send + Sync>>>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self(Arc::new(CancellationState {
            cancelled: AtomicBool::new(false),
            #[cfg(any(unix, windows))]
            next_waker: AtomicU64::new(0),
            #[cfg(any(unix, windows))]
            wakers: Mutex::new(BTreeMap::new()),
        }))
    }

    pub fn cancel(&self) {
        if self.0.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }

        self.wake_registered();
    }

    /// Takes the wake list and calls every waker in it exactly once.
    ///
    /// Only the first caller of [`Self::cancel`] gets here, and it takes the
    /// map, so a waker registered before cancellation is called once and a
    /// waker registered after it never lands in the map at all.
    #[cfg(any(unix, windows))]
    fn wake_registered(&self) {
        let wakers = self
            .0
            .wakers
            .lock()
            .map(|mut wakers| {
                std::mem::take(&mut *wakers)
                    .into_values()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for waker in wakers {
            waker();
        }
    }

    /// On the browser target nothing registers a waker, so cancellation is
    /// the flag alone.
    ///
    /// This exists so `cancel` keeps a statement after its early return on
    /// every target. Inlining the wake under a native gate left the return as
    /// the whole body elsewhere, which is a lint the browser build now runs.
    #[cfg(not(any(unix, windows)))]
    fn wake_registered(&self) {}

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    /// Call `waker` once when this token is cancelled.
    ///
    /// A token that is already cancelled calls `waker` before returning and
    /// yields no registration. Otherwise the waker stays registered until the
    /// returned registration drops, so a caller that stops waiting withdraws
    /// it.
    #[cfg(any(unix, windows))]
    #[must_use = "dropping the registration withdraws the waker"]
    pub fn register_waker(
        &self,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<CancellationRegistration> {
        if self.is_cancelled() {
            waker();
            return None;
        }

        let Ok(mut wakers) = self.0.wakers.lock() else {
            waker();
            return None;
        };
        if self.is_cancelled() {
            drop(wakers);
            waker();
            return None;
        }

        let id = self.0.next_waker.fetch_add(1, Ordering::Relaxed);
        wakers.insert(id, waker);
        Some(CancellationRegistration {
            state: Arc::downgrade(&self.0),
            id,
        })
    }
}

#[cfg(any(unix, windows))]
pub struct CancellationRegistration {
    state: Weak<CancellationState>,
    id: u64,
}

#[cfg(any(unix, windows))]
impl Drop for CancellationRegistration {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade()
            && let Ok(mut wakers) = state.wakers.lock()
        {
            wakers.remove(&self.id);
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestartDisposition {
    Never,
    CallerDecides,
}

#[derive(Clone, Debug)]
pub struct WaitPolicy {
    owner: String,
    cancellation: Cancellation,
    restart: RestartDisposition,
}

impl WaitPolicy {
    pub fn new(
        owner: impl Into<String>,
        cancellation: Cancellation,
        restart: RestartDisposition,
    ) -> Result<Self, SpecError> {
        let owner = owner.into();
        if owner.trim().is_empty() {
            return Err(SpecError::EmptyWaitOwner);
        }

        Ok(Self {
            owner,
            cancellation,
            restart,
        })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn cancellation(&self) -> &Cancellation {
        &self.cancellation
    }

    pub fn restart(&self) -> RestartDisposition {
        self.restart
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdinPolicy {
    Inherit,
    Null,
    /// Give the caller one supervised write before waiting for completion.
    Pipe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamPolicy {
    Inherit,
    Null,
    /// Drain the whole stream while retaining only its bounded prefix.
    Capture {
        limit: usize,
    },
    /// Terminate the process group once the bounded prefix fills.
    CaptureBounded {
        limit: usize,
    },
    /// Drain the whole stream while retaining only its bounded suffix.
    CaptureTail {
        limit: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Output {
    stdin: StdinPolicy,
    stdout: StreamPolicy,
    stderr: StreamPolicy,
}

impl Output {
    pub const fn new(stdin: StdinPolicy, stdout: StreamPolicy, stderr: StreamPolicy) -> Self {
        Self {
            stdin,
            stdout,
            stderr,
        }
    }

    pub const fn stdin(self) -> StdinPolicy {
        self.stdin
    }

    pub const fn stdout(self) -> StreamPolicy {
        self.stdout
    }

    pub const fn stderr(self) -> StreamPolicy {
        self.stderr
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Redaction {
    None,
    All,
    Selected {
        arguments: BTreeSet<usize>,
        environment: BTreeSet<OsString>,
    },
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub argv: Argv,
    pub cwd: PathBuf,
    pub environment: Environment,
    pub group: ProcessGroup,
    pub deadlines: Deadlines,
    pub wait: WaitPolicy,
    pub output: Output,
    pub redaction: Redaction,
    pub grace: Duration,
}

impl Spec {
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpecError {
    EmptyProgram,
    EmptyEnvironmentKey,
    DuplicateEnvironmentKey(OsString),
    ZeroOutputDrainDeadline,
    ZeroReapDeadline,
    EmptyWaitOwner,
}

impl fmt::Display for SpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyProgram => formatter.write_str("argv program must not be empty"),
            Self::EmptyEnvironmentKey => formatter.write_str("environment keys must not be empty"),
            Self::DuplicateEnvironmentKey(key) => {
                write!(formatter, "environment key {key:?} is duplicated")
            }
            Self::ZeroOutputDrainDeadline => {
                formatter.write_str("output-drain deadline must be nonzero")
            }
            Self::ZeroReapDeadline => formatter.write_str("reap deadline must be nonzero"),
            Self::EmptyWaitOwner => formatter.write_str("wait owner must not be empty"),
        }
    }
}

impl Error for SpecError {}

#[cfg(test)]
mod tests {
    use super::{Environment, SpecError};

    #[test]
    fn environment_rejects_duplicate_keys() {
        let error = Environment::new([("ONE", "first"), ("ONE", "second")])
            .expect_err("duplicate environment keys must be rejected");

        assert_eq!(error, SpecError::DuplicateEnvironmentKey("ONE".into()));
    }

    #[cfg(unix)]
    #[test]
    fn environment_records_as_text_and_keeps_bytes_that_are_not() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let raw = OsString::from_vec(vec![b'r', 0xff, b'w']);
        let environment = Environment::new([
            (OsString::from("PATH"), OsString::from("/bin")),
            (OsString::from("RAW"), raw),
        ])
        .expect("distinct keys");

        let recorded = serde_json::to_string(&environment).expect("an environment records");
        assert_eq!(
            recorded,
            r#"[["PATH","/bin"],["RAW",{"bytes":[114,255,119]}]]"#
        );
        let read: Environment = serde_json::from_str(&recorded).expect("the record reads back");
        assert_eq!(read, environment);
        assert!(
            serde_json::from_str::<Environment>(r#"[["ONE","1"],["ONE","2"]]"#).is_err(),
            "a record with a duplicate key is refused as the constructor refuses it"
        );
    }

    #[cfg(windows)]
    #[test]
    fn environment_records_as_text_and_keeps_wide_units_that_are_not() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        // An unpaired surrogate is a valid Windows name but not Unicode.
        let raw = OsString::from_wide(&[u16::from(b'r'), 0xd800, u16::from(b'w')]);
        let environment = Environment::new([
            (OsString::from("PATH"), OsString::from(r"C:\bin")),
            (OsString::from("RAW"), raw),
        ])
        .expect("distinct keys");

        let recorded = serde_json::to_string(&environment).expect("an environment records");
        assert_eq!(
            recorded,
            r#"[["PATH","C:\\bin"],["RAW",{"wide":[114,55296,119]}]]"#
        );
        let read: Environment = serde_json::from_str(&recorded).expect("the record reads back");
        assert_eq!(read, environment);
    }
}
