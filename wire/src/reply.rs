//! The result triad: one reply names the command and carries one outcome.

use std::fmt;

use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::envelope::{Seq, WireVersion};
use crate::fact::{AnyFact, FactKind};
use crate::ident::{CanonicalUuid, Digest};
use crate::manifest::ReleaseTarget;
use crate::output::{Bounded, ClippedText};
use crate::{Actor, Envelope, Instant};

/// An inclusive range of committed journal positions.
///
/// Decoding refuses a reversed range: WIRE names this an inclusive range, and
/// a `last` below `first` does not describe one. In-process construction is
/// the resident's own business, so the fields stay public and
/// [`is_ordered`](Self::is_ordered) remains available to check one before it
/// is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SeqRangeShape")]
pub struct SeqRange {
    /// First position committed by the transaction.
    pub first: Seq,
    /// Last position committed by the transaction.
    pub last: Seq,
}

impl SeqRange {
    /// True when the range is ordered.
    #[must_use]
    pub const fn is_ordered(self) -> bool {
        self.first.get() <= self.last.get()
    }
}

/// The decoded shape of a [`SeqRange`], before its ordering is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SeqRangeShape {
    first: Seq,
    last: Seq,
}

/// A decoded range whose `last` sits below its `first`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("committed seq range is inclusive, so last must not precede first: {first}..{last}")]
pub struct SeqRangeError {
    /// First position as decoded.
    pub first: Seq,
    /// Last position as decoded.
    pub last: Seq,
}

impl TryFrom<SeqRangeShape> for SeqRange {
    type Error = SeqRangeError;

    fn try_from(shape: SeqRangeShape) -> Result<Self, Self::Error> {
        let range = Self {
            first: shape.first,
            last: shape.last,
        };
        if range.is_ordered() {
            Ok(range)
        } else {
            Err(SeqRangeError {
                first: shape.first,
                last: shape.last,
            })
        }
    }
}

/// Where a successful result sits relative to the journal.
///
/// A mutating command commits a range, a read-only command answers from a
/// snapshot, and a daemonless command carries neither coordinate. The two
/// positioned forms use distinct keys, so a reader never guesses which it
/// received and no caller fabricates a sequence for a journal-free answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyPosition {
    /// Inclusive range committed by a mutating transaction.
    Committed(SeqRange),
    /// Snapshot cursor answered by a read-only command.
    Snapshot(Seq),
    /// Direct daemonless answer carrying no journal position.
    Direct {},
}

impl Serialize for ReplyPosition {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut position =
            serializer.serialize_map(Some(usize::from(!matches!(self, Self::Direct {}))))?;
        match self {
            Self::Committed(range) => position.serialize_entry("seq", range)?,
            Self::Snapshot(cursor) => position.serialize_entry("cursor", cursor)?,
            Self::Direct {} => {}
        }
        position.end()
    }
}

impl<'de> Deserialize<'de> for ReplyPosition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_from(ReplyPositionShape::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

/// The two optional coordinate keys before their exclusivity is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyPositionShape {
    #[serde(default, deserialize_with = "crate::optional::present")]
    seq: Option<SeqRange>,
    #[serde(default, deserialize_with = "crate::optional::present")]
    cursor: Option<Seq>,
}

/// A reply position that carries both journal coordinates.
#[derive(Debug, thiserror::Error)]
#[error("reply position cannot carry both seq and cursor")]
struct ReplyPositionShapeError;

impl TryFrom<ReplyPositionShape> for ReplyPosition {
    type Error = ReplyPositionShapeError;

    fn try_from(shape: ReplyPositionShape) -> Result<Self, Self::Error> {
        match (shape.seq, shape.cursor) {
            (Some(range), None) => Ok(Self::Committed(range)),
            (None, Some(cursor)) => Ok(Self::Snapshot(cursor)),
            (None, None) => Ok(Self::Direct {}),
            (Some(_), Some(_)) => Err(ReplyPositionShapeError),
        }
    }
}

/// A typed result and its relationship to the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply<B> {
    /// Committed range, read snapshot, or an omitted daemonless position.
    #[serde(flatten)]
    pub position: ReplyPosition,
    /// Kind-specific result.
    pub body: B,
}

/// Product, executable, protocol, authority, and app-generation identity.
///
/// These values are one machine document rather than independently synchronized
/// prose. The daemonless `version` leaf and app bootstrap both use this
/// shape, with the app field naming the generation served at their read
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    /// Canonical product version compiled from distribution authority.
    pub product: crate::manifest::ProductVersion,
    /// Digest of the native executable for its release target.
    pub binary: Digest,
    /// The one crossing wire generation this build speaks.
    pub wire: WireVersion,
    /// Finalized journal migration schema this build observed without migrating it.
    pub journal: u32,
    /// Internal browser/resident app protocol.
    pub app_protocol: crate::hello::AppProtocolVersion,
    /// Identity of the complete app generation served by this build.
    pub app: Digest,
}

/// RTK savings reported from one exact agent history database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RtkGainReport {
    /// Agent identity supplied by the operator.
    pub agent: String,
    /// Exact database path selected after repository-wide resolution.
    pub database: String,
    /// Standard output produced by `rtk gain` for that database.
    pub output: String,
}

/// Runtime residue retired for lanes the resident no longer seats, and rack
/// extractions of generations other than the purging executable's own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanePurgeReport {
    /// How many lanes the resident seats; their residue is kept.
    pub seated: u64,
    /// Lanes no longer seated whose residue was retired, in lane order.
    pub purged: Vec<LanePurge>,
    /// Retired rack extraction directories relative to the runtime root, one
    /// per generation the purging executable does not link, in path order.
    pub rack: Vec<String>,
}

/// One unseated lane and the runtime paths retired for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanePurge {
    /// Lane whose residue was retired.
    pub lane: CanonicalUuid,
    /// Retired paths relative to the runtime root, in family order.
    pub paths: Vec<String>,
}

/// Receipt for opening the current resident's published browser surface.
///
/// The operator capability is intentionally absent. It is a socket credential,
/// not browser state, and must never cross the native command's output boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppOpen {
    /// Exact loopback HTTP origin read from owner-only discovery.
    pub endpoint: String,
    /// Resident generation whose discovery was still current at launch.
    pub epoch: CanonicalUuid,
}

/// The first-run page: where the console answers and how to seat an agent.
impl fmt::Display for AppOpen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "Spice is running at {}", self.endpoint)?;
        write!(
            formatter,
            "Seat an agent lane with `spice lane add [WORKTREE]`."
        )
    }
}

/// Receipt for removing one repository's installed Windows lifecycle owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeeperRemoved {
    /// Task Scheduler path the registration held.
    pub task: String,
    /// Whether a resident was serving and was drained on the way out.
    pub drained: bool,
}

impl fmt::Display for KeeperRemoved {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Removed lifecycle owner {}; {}",
            self.task,
            if self.drained {
                "the resident it kept was drained"
            } else {
                "no resident was serving"
            }
        )
    }
}

/// Receipt for one native window session over the console, answered once
/// the operator closes the window.
///
/// Like [`AppOpen`], it never carries the operator capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppClosed {
    /// Exact loopback HTTP origin the window showed.
    pub endpoint: String,
    /// Resident generation whose discovery admitted that origin.
    pub epoch: CanonicalUuid,
    /// What the window's close did to the resident behind it.
    pub resident: AppResident,
}

/// Whether `spice app` owned the resident it showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppResident {
    /// The command started the resident for this window and drained it on close.
    Drained,
    /// The command joined a resident already serving and left it running.
    Running,
}

impl fmt::Display for AppClosed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.resident {
            AppResident::Drained => write!(
                formatter,
                "Spice app closed; the resident it started at {} was drained.",
                self.endpoint
            ),
            AppResident::Running => write!(
                formatter,
                "Spice app closed; the resident at {} keeps running. `spice down` stops it.",
                self.endpoint
            ),
        }
    }
}

/// Deterministic environment and lifecycle receipt returned by `spice demo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DemoAnswer {
    /// Canonical disposable repository root.
    pub root: String,
    /// Canned transcript seeded beneath the repository root.
    pub transcript: String,
    /// Whether this invocation only seeded or also ran the app.
    pub mode: String,
    /// Whether the root remains available or was removed after clean app shutdown.
    pub lifecycle: String,
    /// Exact loopback origin admitted for app mode.
    ///
    /// Present whenever the app ran, including the default run that opens no
    /// browser, so a deliberate open is always one copy away.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub endpoint: Option<String>,
    /// Shell-safe command that opens this exact seeded root.
    pub launch_command: String,
    /// Shell-safe command that removes this exact disposable root.
    pub remove_command: String,
}

/// Authenticated identities and replay counts for one offline restore bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreVerification {
    /// Domain-separated identity of the canonical backup manifest.
    pub bundle: Digest,
    /// Crossing schema admitted by the canonical manifest.
    pub wire: WireVersion,
    /// Finalized schema admitted from the authority member.
    pub journal_schema: u32,
    /// Domain-separated identity of the SQLite authority bytes.
    pub authority: Digest,
    /// Number of authenticated regular-file members.
    pub members: u64,
    /// Facts whose complete replay closure verified.
    pub facts: u64,
    /// Command receipts whose request/answer bindings verified.
    pub receipts: u64,
    /// Current authority heads whose canonical bodies verified.
    pub heads: u64,
    /// Origin/cause thread-index rows whose closure verified.
    pub threads: u64,
}

/// One fact as a bounded page shows it: identity, attribution, and its
/// canonical JSON clipped to the page's text limit, so no single fact can
/// exceed the page's byte budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactRow {
    /// Journal position.
    pub seq: Seq,
    /// Commit instant.
    pub at: Instant,
    /// Registered fact kind.
    pub kind: FactKind,
    /// Principal that caused the append.
    pub actor: Actor,
    /// Position of the immediately provoking fact; absent at a chain root.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub cause: Option<Seq>,
    /// Canonical fact JSON, clipped.
    pub fact: ClippedText,
}

impl FactRow {
    /// The row for `envelope` with its fact text clipped to `max_text` bytes.
    ///
    /// # Panics
    ///
    /// Never: a decoded fact always re-encodes.
    #[must_use]
    pub fn of(envelope: &Envelope<AnyFact>, max_text: usize) -> Self {
        let fact = serde_json::to_string(&envelope.fact).expect("a decoded fact re-encodes");
        Self {
            seq: envelope.seq,
            at: envelope.at,
            kind: envelope.fact.kind(),
            actor: envelope.actor.clone(),
            cause: envelope.cause,
            fact: crate::output::clip(&fact, max_text),
        }
    }
}

impl fmt::Display for FactRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {} {} {}",
            self.seq,
            self.at,
            self.kind.as_str(),
            self.actor
        )?;
        if let Some(cause) = self.cause {
            write!(formatter, " cause={cause}")?;
        }
        write!(formatter, " {}", self.fact)
    }
}

/// One newest-first page of the finalized journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalReport {
    /// Finalized journal schema admitted by the reader.
    pub schema: u32,
    /// Greatest committed position, absent for an empty authority.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub cursor: Option<Seq>,
    /// Number of canonical fact envelopes in the snapshot.
    pub facts: u64,
    /// Newest facts at or before the requested cursor.
    pub entries: Bounded<FactRow>,
}

impl fmt::Display for JournalReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "journal schema {}", self.schema)?;
        if let Some(cursor) = self.cursor {
            write!(formatter, " cursor {cursor}")?;
        }
        write!(formatter, " facts {}", self.facts)?;
        fact_page(formatter, &self.entries)
    }
}

fn fact_page(formatter: &mut fmt::Formatter<'_>, page: &Bounded<FactRow>) -> fmt::Result {
    for row in &page.rows {
        write!(formatter, "\n{row}")?;
    }
    match &page.omission {
        Some(omission) => write!(formatter, "\n{omission}"),
        None => Ok(()),
    }
}

/// Counts proved by a complete read-only authority verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalVerification {
    /// Finalized journal schema that was verified.
    pub schema: u32,
    /// Canonical fact envelopes verified.
    pub facts: u64,
    /// Command receipts verified.
    pub receipts: u64,
    /// Current authority heads verified.
    pub heads: u64,
    /// Derived thread-index rows verified.
    pub threads: u64,
}

/// Counts returned after rebuilding the disposable thread index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalReindex {
    /// Immutable fact envelopes used as the rebuild source.
    pub facts: u64,
    /// Thread rows published by the transaction.
    pub threads: u64,
}

/// Receipt for one complete, authenticated backup bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalBackup {
    /// Operator-selected bundle destination.
    pub output: String,
    /// Authentication and replay proof for the published bundle.
    pub verification: RestoreVerification,
}

/// Digest-pinned preview or completed authority replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalRestore {
    /// Domain-separated identity of this bundle, destination, and predecessor.
    pub plan: Digest,
    /// Repository authority path the plan names.
    pub destination: String,
    /// Identity of the authority present when the plan was made, if any.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub previous: Option<Digest>,
    /// Complete verification of the proposed replacement generation.
    pub verification: RestoreVerification,
    /// True only after the digest-pinned replacement and reopen verification.
    pub applied: bool,
}

/// One verified cause thread reconstructed from immutable envelopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadReport {
    /// Exact sequence or origin reference supplied by the caller.
    pub reference: String,
    /// Root fact of the resolved thread.
    pub root: Seq,
    /// Facts from the root through its descendants, from the requested cursor.
    pub entries: Bounded<FactRow>,
}

impl fmt::Display for ThreadReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "thread {} root {}", self.reference, self.root)?;
        fact_page(formatter, &self.entries)
    }
}

/// One release change derived from first-parent commit history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseNotesEntry {
    /// Task topic, or `/general` when no typed landing names the commit.
    pub topic: String,
    /// Consistently formatted task title or Git commit subject.
    pub subject: String,
    /// Stable task identity shared by repeated phase landings, when declared.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub task: Option<String>,
    /// Every first-parent landing reference contributing to this entry.
    pub references: Vec<String>,
}

/// Deterministic release-note draft or publication document derived from history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseNotes {
    /// Sole release-note document schema emitted by this build.
    pub version: u16,
    /// Domain-separated identity of the exact rendered note bytes.
    pub identity: Digest,
    /// Version the release page names.
    pub release_version: String,
    /// Fully resolved commit whose first-parent ledger was folded.
    pub release_commit: String,
    /// Highest-versioned reachable prior release tag, when one exists.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub previous_tag: Option<String>,
    /// True when absence of a prior tag forced the explicit recent-history window.
    pub bounded_recent_window: bool,
    /// Release entries in first-parent order.
    pub entries: Vec<ReleaseNotesEntry>,
    /// Exact Markdown bytes rendered for review or publication.
    pub document: String,
}

/// Verified identities returned by both daemonless proof leaves.
///
/// This body has no standing or success flag: its existence is the clean
/// outcome. Proof verification admits only clean evidence before producing
/// this document, while a refused replay travels as [`Outcome::Refusal`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofEvidence {
    /// Release tuple covered by the payload, replay, and receipt.
    pub target: ReleaseTarget,
    /// Canonical identity of the receipt-free target payload.
    pub payload: Digest,
    /// Canonical identity of the detached proof receipt.
    pub receipt: Digest,
    /// Canonical identity of the ordered proof battery.
    pub battery: Digest,
    /// Canonical identity of the complete replay verdict.
    pub replay: Digest,
}

/// One post-proof all-target manifest recognized by the resident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedManifest {
    /// Domain-separated identity of the exact canonical manifest bytes.
    pub digest: Digest,
    /// Complete target, payload, receipt, battery, replay, and environment bindings.
    pub manifest: crate::manifest::ReleaseManifest,
}

/// Structured diagnostic shared by refusal forms that report a failed predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefusalDiagnostic {
    /// Stable reason, suitable for matching rather than display alone.
    reason: String,
    /// Failed predicate name, when a witness is what failed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    witness: Option<String>,
    /// Value the predicate required.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    expected: Option<String>,
    /// Value the predicate observed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    observed: Option<String>,
    /// Human diagnostic detail that is not itself a recovery command.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    detail: Option<String>,
}

impl RefusalDiagnostic {
    /// Build one diagnostic from the exact predicate evidence available.
    #[must_use]
    pub fn new(
        reason: impl Into<String>,
        witness: Option<String>,
        expected: Option<String>,
        observed: Option<String>,
    ) -> Self {
        Self {
            reason: reason.into(),
            witness,
            expected,
            observed,
            detail: None,
        }
    }

    /// Attach human detail without claiming it is a runnable recovery.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Stable machine reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Failed predicate name, when present.
    #[must_use]
    pub fn witness(&self) -> Option<&str> {
        self.witness.as_deref()
    }

    /// Required predicate value, when present.
    #[must_use]
    pub fn expected(&self) -> Option<&str> {
        self.expected.as_deref()
    }

    /// Observed predicate value, when present.
    #[must_use]
    pub fn observed(&self) -> Option<&str> {
        self.observed.as_deref()
    }

    /// Human diagnostic detail, when present.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Replace the observed predicate value while retaining the same refusal form.
    pub fn replace_observed(&mut self, observed: Option<String>) -> Option<String> {
        std::mem::replace(&mut self.observed, observed)
    }
}

/// One runnable Spice recovery command.
///
/// The executable is structural rather than caller-authored: every value runs
/// `spice` with these exact arguments, and construction requires one concrete
/// top-level subcommand rather than an absent or prose-only remedy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<String>", into = "Vec<String>")]
pub struct SpiceCommand {
    arguments: Vec<String>,
}

impl SpiceCommand {
    /// Bind a required top-level command and exact trailing arguments to the
    /// one Spice executable.
    ///
    /// # Errors
    ///
    /// Refuses an empty or option-shaped top-level command. This keeps a
    /// repair arm from carrying `spice` alone or a dummy global option.
    pub fn new<I, S, T>(command: S, arguments: I) -> Result<Self, &'static str>
    where
        I: IntoIterator<Item = T>,
        S: Into<String>,
        T: Into<String>,
    {
        let arguments: Vec<String> = std::iter::once(command.into())
            .chain(arguments.into_iter().map(Into::into))
            .collect();
        Self::try_from(arguments)
    }

    /// Exact arguments following the `spice` executable.
    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }
}

impl TryFrom<Vec<String>> for SpiceCommand {
    type Error = &'static str;

    fn try_from(arguments: Vec<String>) -> Result<Self, Self::Error> {
        let Some(command) = arguments.first() else {
            return Err("a recovery command requires one Spice subcommand");
        };
        if command.is_empty()
            || command.starts_with('-')
            || command.chars().any(char::is_whitespace)
        {
            return Err("a recovery command requires one non-option Spice subcommand token");
        }
        if arguments.iter().any(|argument| argument.contains('\0')) {
            return Err("a recovery command cannot contain a NUL byte");
        }
        Ok(Self { arguments })
    }
}

impl From<SpiceCommand> for Vec<String> {
    fn from(command: SpiceCommand) -> Self {
        command.arguments
    }
}

/// The command as one shell-quoted line led by `spice`, so it can be pasted.
impl fmt::Display for SpiceCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let words = std::iter::once("spice").chain(self.arguments.iter().map(String::as_str));
        formatter.write_str(&shell_words::join(words))
    }
}

/// An answer about an event that did not occur.
///
/// A refusal is never a fact. It causes no journal or head mutation; the bus
/// writes a bounded telemetry row instead. Its semantic form is explicit, so
/// absence of a runnable recovery cannot be encoded as an empty remedy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "form", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Refusal {
    /// A caller can repair the refusal by running one exact Spice command.
    Repairable {
        /// Runnable command rendered before the diagnostic.
        command: SpiceCommand,
        /// Predicate that refused the operation.
        diagnostic: Box<RefusalDiagnostic>,
    },
    /// No runnable recovery exists; render only the diagnostic.
    Bare {
        /// Predicate that refused the operation.
        diagnostic: Box<RefusalDiagnostic>,
    },
    /// Invocation syntax itself was invalid; render its one contract statement.
    Invocation {
        /// Stable machine reason.
        reason: String,
        /// Complete invocation-contract statement.
        contract: String,
    },
    /// A gate refused with operator-facing evidence that precedes its summary.
    Evidence {
        /// Stable machine reason.
        reason: String,
        /// Immediate diagnostic evidence. A bounded projection may name an
        /// artifact path that retains complete authoritative detail.
        evidence: String,
        /// Human summary following the evidence.
        summary: String,
    },
}

impl Refusal {
    /// Construct a repairable refusal with one structurally runnable command.
    #[must_use]
    pub fn repairable(command: SpiceCommand, diagnostic: RefusalDiagnostic) -> Self {
        Self::Repairable {
            command,
            diagnostic: Box::new(diagnostic),
        }
    }

    /// Construct a refusal for which no runnable recovery exists.
    #[must_use]
    pub fn bare(diagnostic: RefusalDiagnostic) -> Self {
        Self::Bare {
            diagnostic: Box::new(diagnostic),
        }
    }

    /// Construct one invocation-contract refusal statement.
    #[must_use]
    pub fn invocation(reason: impl Into<String>, contract: impl Into<String>) -> Self {
        Self::Invocation {
            reason: reason.into(),
            contract: contract.into(),
        }
    }

    /// Construct one evidence-first gate refusal.
    #[must_use]
    pub fn evidence(
        reason: impl Into<String>,
        evidence: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self::Evidence {
            reason: reason.into(),
            evidence: evidence.into(),
            summary: summary.into(),
        }
    }

    /// Stable machine reason shared by every refusal form.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Repairable { diagnostic, .. } | Self::Bare { diagnostic } => diagnostic.reason(),
            Self::Invocation { reason, .. } | Self::Evidence { reason, .. } => reason,
        }
    }

    /// Human explanation suitable for non-CLI surfaces that retain reason and
    /// detail as separate fields.
    #[must_use]
    pub fn explanation(&self) -> &str {
        match self {
            Self::Repairable { diagnostic, .. } | Self::Bare { diagnostic } => diagnostic
                .detail()
                .or_else(|| diagnostic.observed())
                .or_else(|| diagnostic.expected())
                .unwrap_or_else(|| diagnostic.reason()),
            Self::Invocation { contract, .. } => contract,
            Self::Evidence { summary, .. } => summary,
        }
    }

    /// Predicate diagnostic carried by repairable and bare forms.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&RefusalDiagnostic> {
        match self {
            Self::Repairable { diagnostic, .. } | Self::Bare { diagnostic } => {
                Some(diagnostic.as_ref())
            }
            Self::Invocation { .. } | Self::Evidence { .. } => None,
        }
    }

    /// Runnable recovery command carried only by the repairable form.
    #[must_use]
    pub fn command(&self) -> Option<&SpiceCommand> {
        match self {
            Self::Repairable { command, .. } => Some(command),
            Self::Bare { .. } | Self::Invocation { .. } | Self::Evidence { .. } => None,
        }
    }

    /// Mutably narrow to the predicate diagnostic carried by diagnostic forms.
    #[must_use]
    pub fn diagnostic_mut(&mut self) -> Option<&mut RefusalDiagnostic> {
        match self {
            Self::Repairable { diagnostic, .. } | Self::Bare { diagnostic } => {
                Some(diagnostic.as_mut())
            }
            Self::Invocation { .. } | Self::Evidence { .. } => None,
        }
    }
}

/// The plane in which a defect was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FaultClass {
    /// A wire, epoch, or identity defect.
    Protocol,
    /// A journal or folds-store defect.
    Store,
    /// A defect outside the process boundary.
    Environment,
}

/// A named protocol, store, or environment defect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fault {
    /// Plane in which the defect was detected.
    pub class: FaultClass,
    /// Stable reason, suitable for matching rather than display alone.
    pub reason: String,
    /// What was observed, in enough detail to act on.
    pub detail: String,
}

/// One tagged outcome carried by a reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Outcome<B> {
    /// The command succeeded.
    Reply(Reply<B>),
    /// The command was refused; nothing was committed.
    Refusal(Refusal),
    /// A defect prevented an answer.
    Fault(Fault),
}

/// The reply frame: the command it answers, the epoch that answered, and one
/// tagged outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response<B> {
    /// Current wire version.
    pub v: WireVersion,
    /// Identity of the command being answered.
    pub id: CanonicalUuid,
    /// Resident epoch that produced the answer.
    pub epoch: CanonicalUuid,
    /// The one outcome.
    pub outcome: Outcome<B>,
}
