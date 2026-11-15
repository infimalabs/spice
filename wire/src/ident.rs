//! Canonical identity spellings shared by every crossing type.
//!
//! WIRE fixes one spelling per durable identity. UUIDs are lower-case
//! `8-4-4-4-12`; instants are RFC3339 UTC with millisecond precision and a
//! trailing `Z`. The `uuid` and `jiff` crates own the underlying primitives,
//! so no leaf chooses its own; these newtypes add only the spelling law,
//! which those crates deliberately do not enforce.
//!
//! # Instants narrow precision and refuse unspellable years
//!
//! An [`Instant`] holds exactly the resolution its spelling carries, because
//! [`Instant::new`] floors to milliseconds instead of storing the sharper
//! timestamp it was handed. Equal wire bytes therefore imply equal values, and
//! `Ord` and `Hash` agree with what `Display` renders.
//!
//! Sharper input is narrowed because `new` is how a clock reading becomes
//! wire-shaped, and clocks normally carry more precision than the wire. Years
//! outside the fixed-width `YYYY` schema are different: no admitted spelling
//! can represent them, so construction refuses them instead of creating a
//! value that can encode but never decode. [`FromStr`] likewise rejects a
//! sharper or extended-year spelling outright rather than repairing it.
//!
//! The narrowing floors toward negative infinity, matching the printer.
//! Truncating toward zero — which is what `Timestamp::as_millisecond` and
//! `RoundMode::Trunc` both do — disagrees with the rendered spelling before
//! 1970: `1969-12-31T23:59:59.999999Z` renders `.999Z` but truncates up to
//! `1970-01-01T00:00:00Z`, reintroducing this very defect in a narrower range.

use std::fmt;
use std::io;
use std::str::FromStr;

use serde::de::{Deserializer, Error as DeError};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

mod digest;

/// A spelling that WIRE refuses.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IdentError {
    /// The text is not the canonical lower-case `8-4-4-4-12` UUID spelling.
    #[error(
        "uuid must be canonical lower-case 8-4-4-4-12; \
         braces, upper case, compact text, and urn forms are refused: {0:?}"
    )]
    Uuid(String),
    /// The text is not RFC3339 UTC with millisecond precision and a `Z` suffix.
    #[error("instant must be RFC3339 UTC with millisecond precision and a trailing Z: {0:?}")]
    Instant(String),
    /// The text is not 64 lower-case hex characters.
    #[error("digest must be 64 lower-case hex characters: {0:?}")]
    Digest(String),
}

/// The class of thing a durable digest stands for.
///
/// identity-law: non-crossing - computation input whose resulting Digest crosses.
///
/// WIRE fixes one algorithm set so no leaf chooses its own, and the domain tag
/// is the half of that set a caller still has to name. The classes are closed
/// here rather than spelled at each call site: a new class is a new variant in
/// this file, which is what stops two leaves from minting the same class under
/// two spellings and calling the results the same identity.
///
/// Every variant leads with the plane its tag places it in. Two planes both
/// want a word like `inventory`, and whichever arrives first would otherwise
/// take the bare noun and leave the second to qualify itself, so the same kind
/// of thing reads as two different kinds. Leading with the plane costs the
/// first arrival nothing and means no later one has to be spelled around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Domain {
    /// The exact authored bytes of one board document.
    BoardDocument,
    /// Stable task identity derived from one canonical authored node.
    BoardTaskIdentity,
    /// The task and workflow heads read by one board document preview.
    BoardBarrier,
    /// The exact texts and deferral one previewed debt sweep files.
    BoardSweepPreview,
    /// A topic mutation bound to the current topic authority.
    TopicPlan,
    /// Existing task placement bound to its statement and scored routing inputs.
    TaskPlacementPlan,
    /// One complete machine-local configuration document.
    ConfigLocal,
    /// One complete observed machine-local configuration mutation plan.
    ConfigPlan,
    /// The canonical inventory document for one journal backup bundle.
    JournalBackupManifest,
    /// The exact bytes of one member in a journal backup bundle.
    JournalBackupMember,
    /// The exact bytes of one task-addressed sidecar artifact.
    TaskArtifactPayload,
    /// One task invitation bound to exact task, claimant, and lane authority.
    TaskInvite,
    /// One complete manifest-referenced task artifact store generation.
    TaskArtifactStore,
    /// The exact restore action pinned before authority replacement.
    JournalRestorePlan,
    /// One canonical exact source link cited by a room contribution.
    RoomSource,
    /// The canonical identity of one retained lane-profile evidence card.
    RoomProfileEvidence,
    /// The normalized text of one task statement item.
    StatementItem,
    /// The payload manifest of one release target.
    DistTargetPayload,
    /// The bytes of one staged artifact.
    DistArtifact,
    /// The bytes of one file selected by an installation plan.
    InstallArtifact,
    /// The complete observed installation plan.
    InstallPlan,
    /// The build pin set a staging run resolved.
    DistPins,
    /// The inventory of what a staging run published.
    DistInventory,
    /// The immutable Git commit selected as one release build source.
    DistSourceCommit,
    /// The exact source archive bytes every target of one release builds from.
    DistSourceArchive,
    /// The complete all-target release manifest admitted after proof.
    DistReleaseManifest,
    /// The complete deterministic release-note input document.
    DistReleaseNotes,
    /// The exact operator-facing release-note bytes shipped with one package.
    DistReleaseNotesContent,
    /// One complete externally installable release package index.
    DistReleasePackage,
    /// The exact deterministic archive uploaded as a release asset.
    DistReleaseAsset,
    /// One operator-previewed publication of an exact sealed manifest.
    DistReleasePlan,
    /// The ordered command inventory one sealed proof replay consumes.
    ProofBattery,
    /// The complete typed result of one sealed proof replay.
    ProofReplay,
    /// The detached receipt binding a replay to one target payload.
    ProofReceipt,
    /// The exact bytes of one admitted native proof runner executable.
    ProofNativeRunner,
    /// The bytes of one file inside a native proof source closure.
    ProofNativeClosureFile,
    /// The vendored source closure one native proof run compiles against.
    ProofNativeClosure,
    /// The bytes of one file inside a gate's jurisdiction.
    GateFile,
    /// The admitted file set of one gate jurisdiction, by content.
    GateTree,
    /// The compiled checks one current gate build can execute.
    GateSet,
    /// The enforcement policy one gate run read.
    GatePolicy,
    /// One resolved value of the tracked configuration a gate run read.
    GateConfiguration,
    /// The process bounds every current gate tool answers to.
    GateBounds,
    /// The complete versioned lower-is-better floor state.
    GateBaseline,
    /// One declared gate tool together with the executable bytes found.
    GateTool,
    /// Every path one gate walk reached, admitted or not, with its reason.
    GateInventory,
    /// One action's complete ordered file and non-file input closure.
    GateInputManifest,
    /// The quantities one gate run measured.
    GateMeasurement,
    /// The complete current identity and ordered outcome of one gate run.
    GateReceipt,
    /// The checked-in disposition document naming every compiled battery arm.
    GateBatteryManifest,
    /// The complete typed result vector one battery run produced.
    GateBatteryReport,
    /// One canonical focused gate-arm report.
    GateFocusedReport,
    /// The Git-common repository authority one gate selection belongs to.
    GateRepositoryAuthority,
    /// One exact staged-index or committed-revision selection.
    GateSelection,
    /// One canonical reusable gate action specification.
    GateActionSpec,
    /// One canonical gate action specification and complete input closure.
    GateActionKey,
    /// The retained bytes of one published battery receipt, exactly as read.
    GateBatteryReceipt,
    /// One immutable passing receipt published by a proof-aware gate runner.
    GatePassReceipt,
    /// One complete pre-commit admission over an exact candidate tree.
    GateAdmission,
    /// One durable single-flight lease for producing gate evidence.
    GateEvidenceLease,
    /// One complete current persistent gate-state ledger.
    GateStateLedger,
    /// One immutable accepted or rejected gate-state transition.
    GateStateReceipt,
    /// The exact tracked and machine-local executable policy documents.
    ConfigExecutableSurface,
    /// The normalized authored prose one taste reminder was derived from.
    TasteProse,
    /// One complete ordered active taste bag.
    TasteBag,
    /// Exact decoded bytes of one admitted browsable media block.
    MediaContent,
    /// Exact UTF-8 text authenticated by one transcript quote target.
    QuoteText,
    /// The exact bytes of one complete document a read exported to a file.
    CliExport,
    /// The exact current text of one task's description or terms, which
    /// a replacement of that text names.
    TaskText,
    /// One capability issued to an admitted principal, which the journal keeps
    /// only as this digest.
    PrincipalCapability,
}

impl Domain {
    /// The frozen tag this class mixes into its digests.
    ///
    /// Tags carry a version because widening what a class covers is a new tag,
    /// never a redefinition of an existing one: a digest already published
    /// under `v1` has to keep meaning what it meant when it was published.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::BoardDocument
            | Self::BoardTaskIdentity
            | Self::BoardBarrier
            | Self::BoardSweepPreview
            | Self::TopicPlan
            | Self::TaskPlacementPlan
            | Self::ConfigLocal
            | Self::ConfigPlan
            | Self::JournalBackupManifest
            | Self::JournalBackupMember
            | Self::TaskArtifactPayload
            | Self::TaskInvite
            | Self::TaskArtifactStore
            | Self::JournalRestorePlan
            | Self::RoomSource
            | Self::RoomProfileEvidence
            | Self::StatementItem
            | Self::TasteProse
            | Self::TasteBag
            | Self::MediaContent
            | Self::QuoteText
            | Self::CliExport
            | Self::TaskText
            | Self::PrincipalCapability => self.product_tag(),
            Self::DistTargetPayload
            | Self::DistArtifact
            | Self::InstallArtifact
            | Self::InstallPlan
            | Self::DistPins
            | Self::DistInventory
            | Self::DistSourceCommit
            | Self::DistSourceArchive
            | Self::DistReleaseManifest
            | Self::DistReleaseNotes
            | Self::DistReleaseNotesContent
            | Self::DistReleasePackage
            | Self::DistReleaseAsset
            | Self::DistReleasePlan => self.distribution_tag(),
            Self::ProofBattery
            | Self::ProofReplay
            | Self::ProofReceipt
            | Self::ProofNativeRunner
            | Self::ProofNativeClosureFile
            | Self::ProofNativeClosure
            | Self::GateFile
            | Self::GateTree
            | Self::GateSet
            | Self::GatePolicy
            | Self::GateConfiguration
            | Self::GateBounds
            | Self::GateBaseline
            | Self::GateTool
            | Self::GateInventory
            | Self::GateInputManifest
            | Self::GateActionSpec
            | Self::GateActionKey => self.verification_input_tag(),
            Self::GateMeasurement
            | Self::GateReceipt
            | Self::GateBatteryManifest
            | Self::GateBatteryReport
            | Self::GateFocusedReport
            | Self::GateRepositoryAuthority
            | Self::GateSelection
            | Self::GateBatteryReceipt
            | Self::GatePassReceipt
            | Self::GateAdmission
            | Self::GateEvidenceLease
            | Self::GateStateLedger
            | Self::GateStateReceipt
            | Self::ConfigExecutableSurface => self.verification_output_tag(),
        }
    }

    const fn product_tag(self) -> &'static str {
        match self {
            Self::BoardDocument => "spice.board.document.v1",
            Self::BoardTaskIdentity => "spice.board.task-identity.v1",
            Self::BoardBarrier => "spice.board.barrier.v1",
            Self::BoardSweepPreview => "spice.board.sweep-preview.v1",
            Self::TopicPlan => "spice.topic.plan.v1",
            Self::TaskPlacementPlan => "spice.task.placement-plan.v1",
            Self::ConfigLocal => "spice.config.local.v1",
            Self::ConfigPlan => "spice.config.plan.v1",
            Self::JournalBackupManifest => "spice.journal.backup-manifest.v1",
            Self::JournalBackupMember => "spice.journal.backup-member.v1",
            Self::TaskArtifactPayload => "spice.task.artifact-payload.v1",
            Self::TaskInvite => "spice.task.invite.v1",
            Self::TaskArtifactStore => "spice.task.artifact-store.v1",
            Self::JournalRestorePlan => "spice.journal.restore-plan.v1",
            Self::RoomSource => "spice.room.source.v1",
            Self::RoomProfileEvidence => "spice.room.profile-evidence.v1",
            Self::StatementItem => "spice.statement-item.v1",
            Self::TasteProse => "spice.taste.prose.v1",
            Self::TasteBag => "spice.taste.bag.v1",
            Self::MediaContent => "spice.media.content.v1",
            Self::QuoteText => "spice.quote.text.v1",
            Self::CliExport => "spice.cli.export.v1",
            Self::TaskText => "spice.task.text.v1",
            Self::PrincipalCapability => "spice.principal.capability.v1",
            _ => panic!("digest-domain tag partition is closed"),
        }
    }

    const fn distribution_tag(self) -> &'static str {
        match self {
            Self::DistTargetPayload => "spice.dist.target-payload.v1",
            Self::DistArtifact => "spice.dist.artifact.v1",
            Self::InstallArtifact => "spice.install.artifact.v1",
            Self::InstallPlan => "spice.install.plan.v1",
            Self::DistPins => "spice.dist.pins.v1",
            Self::DistInventory => "spice.dist.inventory.v1",
            Self::DistSourceCommit => "spice.dist.source-commit.v1",
            Self::DistSourceArchive => "spice.dist.source-archive.v1",
            Self::DistReleaseManifest => "spice.dist.release-manifest.v1",
            Self::DistReleaseNotes => "spice.dist.release-notes.v1",
            Self::DistReleaseNotesContent => "spice.dist.release-notes-content.v1",
            Self::DistReleasePackage => "spice.dist.release-package.v1",
            Self::DistReleaseAsset => "spice.dist.release-asset.v1",
            Self::DistReleasePlan => "spice.dist.release-plan.v1",
            _ => panic!("digest-domain tag partition is closed"),
        }
    }

    const fn verification_input_tag(self) -> &'static str {
        match self {
            Self::ProofBattery => "spice.proof.battery.v1",
            Self::ProofReplay => "spice.proof.replay.v1",
            Self::ProofReceipt => "spice.proof.receipt.v1",
            Self::ProofNativeRunner => "spice.proof.native-runner.v1",
            Self::ProofNativeClosureFile => "spice.proof.native-closure-file.v1",
            Self::ProofNativeClosure => "spice.proof.native-closure.v1",
            Self::GateFile => "spice.gate.file.v1",
            Self::GateTree => "spice.gate.tree.v1",
            Self::GateSet => "spice.gate.set.v1",
            Self::GatePolicy => "spice.gate.policy.v1",
            Self::GateConfiguration => "spice.gate.configuration.v1",
            Self::GateBounds => "spice.gate.bounds.v1",
            Self::GateBaseline => "spice.gate.baseline.v1",
            Self::GateTool => "spice.gate.tool.v1",
            Self::GateInventory => "spice.gate.inventory.v1",
            Self::GateInputManifest => "spice.gate.input-manifest.v1",
            Self::GateActionSpec => "spice.gate.action-spec.v1",
            Self::GateActionKey => "spice.gate.action-key.v1",
            _ => panic!("digest-domain tag partition is closed"),
        }
    }

    const fn verification_output_tag(self) -> &'static str {
        match self {
            Self::GateMeasurement => "spice.gate.measurement.v1",
            Self::GateReceipt => "spice.gate.receipt.v1",
            Self::GateBatteryManifest => "spice.gate.battery-manifest.v1",
            Self::GateBatteryReport => "spice.gate.battery-report.v1",
            Self::GateFocusedReport => "spice.gate.focused-report.v1",
            Self::GateRepositoryAuthority => "spice.gate.repository-authority.v1",
            Self::GateSelection => "spice.gate.selection.v1",
            Self::GateBatteryReceipt => "spice.gate.battery-receipt.v1",
            Self::GatePassReceipt => "spice.gate.pass-receipt.v1",
            Self::GateAdmission => "spice.gate.admission.v1",
            Self::GateEvidenceLease => "spice.gate.evidence-lease.v1",
            Self::GateStateLedger => "spice.gate.state-ledger.v1",
            Self::GateStateReceipt => "spice.gate.state-receipt.v1",
            Self::ConfigExecutableSurface => "spice.config.executable-surface.v1",
            _ => panic!("digest-domain tag partition is closed"),
        }
    }
}

/// A durable content digest with exactly one admitted spelling.
///
/// Every digest WIRE names is BLAKE3 over domain-tagged input, so the same
/// bytes under two domains never collide. [`Digest::compute`] is how one is
/// minted; [`FromStr`] only re-admits a spelling that already crossed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest(String);

impl Digest {
    /// Character length of the one admitted spelling.
    const SPELLING_LEN: usize = 64;

    /// Compute the digest of these bytes under one domain.
    ///
    /// The hash covers the domain tag, then a NUL, then the bytes. No tag
    /// contains a NUL, so the first NUL in that input is always the separator
    /// and the pair can be read back off it unambiguously. Cross-domain
    /// collision is therefore impossible by framing rather than merely
    /// improbable by hash strength: two different `(domain, bytes)` pairs never
    /// produce the same hashed input in the first place.
    #[must_use]
    pub fn compute(domain: Domain, bytes: &[u8]) -> Self {
        Self(digest::compute(domain, bytes).to_hex().as_str().to_owned())
    }

    /// Stream at most `limit` body bytes into one domain-separated digest.
    ///
    /// # Errors
    ///
    /// Refuses a zero limit, a reader failure, or the first byte beyond the
    /// admitted limit. The returned length counts only complete body bytes.
    pub fn compute_reader(
        domain: Domain,
        reader: impl io::Read,
        limit: u64,
    ) -> io::Result<(u64, Self)> {
        digest::compute_reader(domain, reader, limit)
            .map(|(length, digest)| (length, Self(digest.to_hex().as_str().to_owned())))
    }

    /// Copy and hash at most `limit` body bytes through one fixed buffer.
    ///
    /// # Errors
    ///
    /// Refuses a zero limit, source or destination failure, or the first byte
    /// beyond the admitted limit. A successful digest covers exactly the bytes
    /// accepted by `writer`.
    pub fn copy_reader(
        domain: Domain,
        reader: impl io::Read,
        writer: &mut dyn io::Write,
        limit: u64,
    ) -> io::Result<(u64, Self)> {
        digest::copy_reader(domain, reader, writer, limit)
            .map(|(length, digest)| (length, Self(digest.to_hex().as_str().to_owned())))
    }

    /// Hash every byte `produce` writes while forwarding it to `writer`.
    ///
    /// The result equals [`Digest::compute`] over the same bytes, without the
    /// producer ever holding them all at once.
    ///
    /// # Errors
    ///
    /// Returns the first failure `produce` or `writer` reports.
    pub fn write_through<W: io::Write>(
        domain: Domain,
        writer: W,
        produce: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
    ) -> io::Result<(W, u64, Self)> {
        let mut hashing = digest::HashingWriter::new(domain, writer);
        produce(&mut hashing)?;
        let (writer, length, digest) = hashing.finish();
        Ok((writer, length, Self(digest.to_hex().as_str().to_owned())))
    }

    /// The digest as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this text is the one spelling WIRE admits.
    ///
    /// [`Digest::compute`] and [`FromStr`] share this predicate so a minted
    /// digest and a decoded one cannot disagree about what canonical means.
    fn admits(text: &str) -> bool {
        text.len() == Self::SPELLING_LEN
            && text.bytes().all(|byte| {
                byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
            })
    }
}

impl FromStr for Digest {
    type Err = IdentError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if Self::admits(text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(IdentError::Digest(text.to_owned()))
        }
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// A UUID with exactly one admitted spelling.
///
/// [`uuid::Uuid::parse_str`] accepts braces, upper case, compact text, and
/// urn prefixes. WIRE admits none of those, so parsing validates the literal
/// shape before delegating the hex decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalUuid(uuid::Uuid);

impl CanonicalUuid {
    /// Byte length of the one admitted spelling.
    const SPELLING_LEN: usize = 36;
    /// Indices carrying the four separators.
    const HYPHENS: [usize; 4] = [8, 13, 18, 23];

    /// Wrap an already-decoded UUID; rendering always uses the canonical form.
    #[must_use]
    pub const fn new(inner: uuid::Uuid) -> Self {
        Self(inner)
    }

    /// The underlying identity.
    #[must_use]
    pub const fn get(self) -> uuid::Uuid {
        self.0
    }

    /// True when `text` is the canonical lower-case `8-4-4-4-12` spelling.
    ///
    /// The check is deliberately literal: length, separator positions, and
    /// lower-case hex everywhere else. Anything a looser parser would repair
    /// is refused here instead.
    fn is_canonical(text: &str) -> bool {
        if text.len() != Self::SPELLING_LEN {
            return false;
        }
        text.bytes().enumerate().all(|(index, byte)| {
            if Self::HYPHENS.contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit())
            }
        })
    }
}

impl FromStr for CanonicalUuid {
    type Err = IdentError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if !Self::is_canonical(text) {
            return Err(IdentError::Uuid(text.to_owned()));
        }
        uuid::Uuid::parse_str(text)
            .map(Self)
            .map_err(|_| IdentError::Uuid(text.to_owned()))
    }
}

impl fmt::Display for CanonicalUuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0.as_hyphenated())
    }
}

impl Serialize for CanonicalUuid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for CanonicalUuid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// An instant with exactly one admitted spelling.
///
/// Deadlines persist as these instants; the waits they arm run on monotonic
/// clocks elsewhere. Identifiers never double as instants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(jiff::Timestamp);

impl Instant {
    /// Fractional digits the one admitted spelling carries.
    const MILLIS: u8 = 3;
    /// Byte length of the one admitted spelling, `MILLIS` digits included.
    const SPELLING_LEN: usize = 24;
    /// Every fixed byte the spelling carries, by index.
    const SEPARATORS: [(usize, u8); 7] = [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'.'),
        (23, b'Z'),
    ];

    /// The repository's one indefinite horizon: the lease of a claim iced on
    /// human approval and the hold a deferral carries when no time wakes it.
    pub const INDEFINITE: &str = "2099-12-31T23:59:59.999Z";

    /// [`Self::INDEFINITE`] as a typed instant.
    #[must_use]
    pub fn indefinite() -> Self {
        Self::INDEFINITE
            .parse()
            .expect("the canonical indefinite horizon is a valid wire instant")
    }

    /// Whether this is the indefinite horizon: a hold that no time ends.
    #[must_use]
    pub fn is_indefinite(self) -> bool {
        self == Self::indefinite()
    }

    /// Wrap a timestamp, flooring it to the millisecond the spelling carries.
    ///
    /// A timestamp built elsewhere in the workspace may carry more resolution
    /// than the wire spells; the surplus is dropped here rather than at render,
    /// so the stored value is the one the spelling denotes.
    ///
    /// # Errors
    ///
    /// Returns [`IdentError::Instant`] when the narrowed timestamp falls
    /// outside the fixed-width `YYYY` range the wire admits.
    pub fn new(inner: jiff::Timestamp) -> Result<Self, IdentError> {
        let floored = inner
            .round(
                jiff::TimestampRound::new()
                    .smallest(jiff::Unit::Millisecond)
                    .mode(jiff::RoundMode::Floor),
            )
            .expect("flooring to milliseconds stays inside the timestamp range");
        let instant = Self(floored);
        let spelling = instant.to_string();
        if Self::is_canonical(&spelling) {
            Ok(instant)
        } else {
            Err(IdentError::Instant(spelling))
        }
    }

    /// The underlying timestamp.
    #[must_use]
    pub const fn get(self) -> jiff::Timestamp {
        self.0
    }

    /// True when `text` is the canonical `YYYY-MM-DDTHH:MM:SS.mmmZ` spelling.
    ///
    /// The check is deliberately literal, like the UUID one: fixed bytes at
    /// fixed indices and digits everywhere else. `jiff` accepts offsets, a
    /// lower-case `z`, a space in place of the `T`, and any fractional
    /// precision; WIRE admits none of those, so the shape is settled before
    /// the parse rather than repaired by it.
    fn is_canonical(text: &str) -> bool {
        if text.len() != Self::SPELLING_LEN {
            return false;
        }
        text.bytes().enumerate().all(|(index, byte)| {
            match Self::SEPARATORS.iter().find(|(at, _)| *at == index) {
                Some((_, fixed)) => byte == *fixed,
                None => byte.is_ascii_digit(),
            }
        })
    }
}

impl FromStr for Instant {
    type Err = IdentError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if !Self::is_canonical(text) {
            return Err(IdentError::Instant(text.to_owned()));
        }
        let timestamp = text
            .parse::<jiff::Timestamp>()
            .map_err(|_| IdentError::Instant(text.to_owned()))?;
        Self::new(timestamp)
    }
}

impl fmt::Display for Instant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let printer = jiff::fmt::temporal::DateTimePrinter::new().precision(Some(Self::MILLIS));
        let mut rendered = String::new();
        printer
            .print_timestamp(&self.0, &mut rendered)
            .map_err(|_| fmt::Error)?;
        formatter.write_str(&rendered)
    }
}

impl Serialize for Instant {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Instant {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}
