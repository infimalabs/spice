use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use listen::{Decoded, Dialect, Locus, LocusLengthError};
use sill::{
    Argv, Deadlines, Environment, Output, ProcessGroup, Redaction, Spec, SpecError, StdinPolicy,
    StreamPolicy, WaitPolicy,
};

/// Stable identity of a session that can be resumed explicitly.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct SessionId(String);

impl SessionId {
    /// Admit a nonempty vendor session identity.
    ///
    /// Surrounding whitespace is not part of an identity, so it is removed
    /// rather than merely looked past: keeping it would let this admit one value
    /// and store another, and the padding would ride into `--resume` where the
    /// vendor matches identities literally.
    ///
    /// # Errors
    ///
    /// Returns [`SessionIdError`] when `value` is empty or whitespace only.
    pub fn new(value: impl Into<String>) -> Result<Self, SessionIdError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(SessionIdError);
        }
        Ok(Self(trimmed.to_owned()))
    }

    /// Vendor spelling of this session identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An empty session identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionIdError;

impl fmt::Display for SessionIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("vendor session identity must not be empty")
    }
}

impl Error for SessionIdError {}

/// One exact session declaration admitted from a selected vendor record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDeclaration {
    /// Identity accepted for an explicit successor resume.
    pub session: SessionId,
    /// Durable record bytes that declared the identity.
    pub locus: Locus,
}

impl SessionDeclaration {
    /// Admit the session declaration carried by one decoded selected record.
    ///
    /// Diagnostic streams and records of every other event kind return no
    /// declaration; a decoder emits at most one declaration per record.
    ///
    /// # Errors
    ///
    /// Returns [`SessionIdError`] when the vendor declared an empty identity.
    pub fn admit(decoded: &Decoded) -> Result<Option<Self>, SessionIdError> {
        decoded
            .events()
            .iter()
            .find_map(|event| match event {
                listen::Event::Session(declaration) => Some(declaration),
                _ => None,
            })
            .map(|declaration| {
                Ok(Self {
                    session: SessionId::new(declaration.identity.clone())?,
                    locus: declaration.locus,
                })
            })
            .transpose()
    }
}

/// Whether a launch begins or resumes an exact session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resume {
    /// Begin a new session.
    Fresh,
    /// Resume the named session rather than whichever session ran last.
    Session(SessionId),
}

impl Resume {
    /// Session identity when this launch resumes one.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Fresh => None,
            Self::Session(session) => Some(session.as_str()),
        }
    }
}

/// Child stream carrying vendor JSONL records.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum RecordStream {
    /// Records arrive on standard output.
    Stdout,
    /// Records arrive on standard error.
    Stderr,
}

/// One vendor-neutral launch and decode contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    argv: Argv,
    dialect: Dialect,
    stream: RecordStream,
    resume: Resume,
    opening: Option<Vec<u8>>,
    model_selection: Option<crate::omp::ModelSelection>,
    first_prompt: Option<String>,
    environment: Vec<(OsString, OsString)>,
}

impl Record {
    pub(crate) fn new<I, S>(
        binary: impl Into<OsString>,
        arguments: I,
        dialect: Dialect,
        stream: RecordStream,
        resume: Resume,
    ) -> Result<Self, SpecError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Ok(Self {
            argv: Argv::new(binary, arguments)?,
            dialect,
            stream,
            resume,
            opening: None,
            model_selection: None,
            first_prompt: None,
            environment: Vec::new(),
        })
    }

    /// State the bytes a supervisor writes to the child's stdin once it exists.
    ///
    /// A vendor driven over a command stream, rather than by its arguments,
    /// opens the conversation this way; the door keeps stdin piped for it.
    pub(crate) fn with_opening(mut self, opening: impl Into<Vec<u8>>) -> Self {
        self.opening = Some(opening.into());
        self
    }

    /// State the model a supervisor selects over the command stream once the
    /// vendor lists what it serves, ahead of the first turn.
    pub(crate) fn with_model_selection(mut self, selection: crate::omp::ModelSelection) -> Self {
        self.model_selection = Some(selection);
        self
    }

    /// State the message that starts the first turn, which a supervisor
    /// writes after [`Self::opening`] once the vendor has answered the host
    /// scheme registration that decides how the message names reads and the
    /// [`Self::model_selection`] it runs on.
    pub(crate) fn with_first_prompt(mut self, prompt: String) -> Self {
        self.first_prompt = Some(prompt);
        self
    }

    /// State the variables the vendor contract itself adds to the child.
    ///
    /// A vendor that exports nothing of its own to the tools it runs has the
    /// lane carry its session identity this way.
    pub(crate) fn with_environment(
        mut self,
        entries: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Self {
        self.environment = entries.into_iter().collect();
        self
    }

    /// Variables the vendor contract adds to the child environment.
    #[must_use]
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    /// Configured executable.
    #[must_use]
    pub fn binary(&self) -> &OsStr {
        self.argv.program()
    }

    /// Exact arguments following the executable.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        self.argv.arguments()
    }

    /// Stream carrying records.
    #[must_use]
    pub const fn stream(&self) -> RecordStream {
        self.stream
    }

    /// Decoder selected for those records.
    #[must_use]
    pub const fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// Fresh or exact-session resume disposition.
    #[must_use]
    pub const fn resume(&self) -> &Resume {
        &self.resume
    }

    /// Bytes written to the child's stdin as soon as it exists, when the
    /// vendor is driven over a command stream rather than by its arguments.
    #[must_use]
    pub fn opening(&self) -> Option<&[u8]> {
        self.opening.as_deref()
    }

    /// The model the supervisor selects once the vendor lists what it serves,
    /// when the vendor takes its model over the command stream.
    #[must_use]
    pub const fn model_selection(&self) -> Option<&crate::omp::ModelSelection> {
        self.model_selection.as_ref()
    }

    /// The message that starts the first turn, written after the opening once
    /// the vendor has answered its host scheme registration and model selection.
    #[must_use]
    pub fn first_prompt(&self) -> Option<&str> {
        self.first_prompt.as_deref()
    }

    /// Build the sole process-door specification for this record contract.
    ///
    /// Stdin stays closed unless the vendor opens a command stream, in which
    /// case the door pipes it and the supervisor writes [`Self::opening`].
    #[must_use]
    pub fn spec(&self, process: &Process) -> Spec {
        let stdin = if self.opening.is_some() {
            StdinPolicy::Pipe
        } else {
            StdinPolicy::Null
        };
        process.spec(self.argv.clone(), stdin)
    }

    /// Decode one selected-stream record already committed to tape.
    ///
    /// Bytes from the other child stream remain raw diagnostics and return
    /// `None`; this module never attempts to parse them itself.
    ///
    /// # Errors
    ///
    /// Returns [`LocusLengthError`] when the selected record's locus does not
    /// cover exactly the submitted bytes.
    pub fn decode(
        &self,
        stream: RecordStream,
        bytes: &[u8],
        locus: Locus,
    ) -> Result<Option<Decoded>, LocusLengthError> {
        if stream != self.stream {
            return Ok(None);
        }
        listen::decode(self.dialect, bytes, locus).map(Some)
    }

    /// Whether one record of the selected stream is a partial that stream
    /// restates whole later, so the capture may leave it off the tape.
    #[must_use]
    pub fn restated(&self, stream: RecordStream, bytes: &[u8]) -> bool {
        stream == self.stream && listen::restated(self.dialect, bytes)
    }
}

/// Vendor-neutral process settings supplied by lane supervision.
#[derive(Clone, Debug)]
pub struct Process {
    cwd: PathBuf,
    environment: Environment,
    deadlines: Deadlines,
    wait: WaitPolicy,
    capture_limit: usize,
    grace: Duration,
}

impl Process {
    /// Build process settings shared by every vendor.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when capture is unbounded by a positive limit.
    pub fn new(
        cwd: impl Into<PathBuf>,
        environment: Environment,
        deadlines: Deadlines,
        wait: WaitPolicy,
        capture_limit: usize,
        grace: Duration,
    ) -> Result<Self, ProcessError> {
        if capture_limit == 0 {
            return Err(ProcessError::ZeroCaptureLimit);
        }
        Ok(Self {
            cwd: cwd.into(),
            environment,
            deadlines,
            wait,
            capture_limit,
            grace,
        })
    }

    /// Working directory used for the child.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn spec(&self, argv: Argv, stdin: StdinPolicy) -> Spec {
        Spec {
            argv,
            cwd: self.cwd.clone(),
            environment: self.environment.clone(),
            group: ProcessGroup::New,
            deadlines: self.deadlines,
            wait: self.wait.clone(),
            output: Output::new(
                stdin,
                StreamPolicy::Capture {
                    limit: self.capture_limit,
                },
                StreamPolicy::Capture {
                    limit: self.capture_limit,
                },
            ),
            redaction: Redaction::All,
            grace: self.grace,
        }
    }
}

/// Invalid common process settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessError {
    /// Capturing no bytes would make the selected record stream unusable.
    ZeroCaptureLimit,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroCaptureLimit => formatter.write_str("vendor capture limit must be nonzero"),
        }
    }
}

impl Error for ProcessError {}
