//! Distribution payloads and release evidence shared by dist and proof.

use std::cmp::Ordering;
use std::fmt;
use std::io::{self, Read, Write};
use std::str::FromStr;

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use serde::de::{Deserializer, Error as _};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use crate::{Digest, WireVersion};

pub mod renderer;

const OCI_SHA256_HEX_LEN: usize = 64;

/// The sole current journal backup bundle schema.
pub const JOURNAL_BACKUP_VERSION: u16 = 1;
/// Most members one journal backup bundle carries besides its manifest.
pub const JOURNAL_BACKUP_MAX_MEMBERS: usize = 4096;
/// Most bytes one journal backup bundle carries, its manifest and every
/// member counted together. Backup capture and restore admission share it.
pub const JOURNAL_BACKUP_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// One regular file authenticated by a journal backup manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JournalBackupMember {
    /// Canonical slash-separated path relative to the bundle root.
    pub path: String,
    /// POSIX permission bits, masked to `0o7777`, on every platform. Windows
    /// observes them as the projection of the member's DACL onto owner, group,
    /// and other, and restores only owner-only modes; it refuses a member whose
    /// mode grants group, other, or special bits rather than change it.
    pub mode: u32,
    /// Exact byte length of the member.
    pub bytes: u64,
    /// Domain-separated identity of the member bytes.
    pub digest: Digest,
}

/// Complete immutable inventory for one journal backup bundle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JournalBackupManifest {
    /// Current bundle schema.
    pub version: u16,
    /// Current crossing schema required to interpret the authority.
    pub wire: WireVersion,
    /// Finalized journal schema carried by the authority member.
    pub journal_schema: u32,
    /// Member path containing the SQLite authority.
    pub authority: String,
    /// Every regular file other than this manifest, ordered by path.
    pub members: Vec<JournalBackupMember>,
}

/// The sole current detached proof receipt schema.
pub const PROOF_RECEIPT_VERSION: u16 = 1;
/// The sole current target payload schema.
pub const TARGET_PAYLOAD_VERSION: u16 = 1;
/// The sole current deterministic macOS symbol artifact schema.
pub const MAC_SYMBOL_ARTIFACT_VERSION: u16 = 1;
/// Most encoded bytes one compressed macOS symbol artifact may carry.
pub const MAC_SYMBOL_ARTIFACT_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Most decoded bytes the two published dSYM members may carry together.
pub const MAC_SYMBOL_RAW_BYTES_LIMIT: u64 = 384 * 1024 * 1024;
/// Most decoded bytes either published dSYM member may carry.
pub const MAC_SYMBOL_MEMBER_BYTES_LIMIT: u64 = MAC_SYMBOL_RAW_BYTES_LIMIT;
const MAC_SYMBOL_DEFLATE_LEVEL: u32 = 6;
const MAC_SYMBOL_MAGIC: &[u8] = b"spice-macos-symbol-v1\n";
const MAC_SYMBOL_HEADER_MAX_BYTES: usize = 4096;
const MACHO_UUID_LENGTH: usize = 36;
const MACHO_UUID_HYPHENS: [usize; 4] = [8, 13, 18, 23];

/// Required shell files; generation-declared renderer files complete the inventory.
pub const APP_FILES: [&str; 14] = [
    APP_HTML,
    APP_MODULE,
    APP_STYLE,
    SPICE_SIDEBAR_STYLE,
    MOSAIC_STYLE,
    TEAM_CONTROLS_STYLE,
    LANE_MOSAIC_STYLE,
    CHROME_STYLE,
    COMPOSER_STYLE,
    COMPOSE_OVERLAY_STYLE,
    WORK_STYLE,
    WORK_MAP_STYLE,
    RESPONSIVE_STYLE,
    APP_WASM,
];
/// Browser application HTML shell.
pub const APP_HTML: &str = "app.html";
/// Browser application JavaScript module.
pub const APP_MODULE: &str = "app.module";
/// Shared browser application stylesheet.
pub const APP_STYLE: &str = "style.css";
/// Spice sidebar and team-arrangement stylesheet.
pub const SPICE_SIDEBAR_STYLE: &str = "spice_sidebar.css";
/// Mosaic workspace stylesheet.
pub const MOSAIC_STYLE: &str = "mosaic.css";
/// Team composer controls stylesheet.
pub const TEAM_CONTROLS_STYLE: &str = "team_controls.css";
/// Lane mosaic plane and message card stylesheet.
pub const LANE_MOSAIC_STYLE: &str = "lane_mosaic.css";
/// Browser chrome stylesheet.
pub const CHROME_STYLE: &str = "chrome.css";
/// Composer stylesheet.
pub const COMPOSER_STYLE: &str = "composer.css";
/// Global composer left-overlay stylesheet.
pub const COMPOSE_OVERLAY_STYLE: &str = "compose_overlay.css";
/// Global Work page stylesheet.
pub const WORK_STYLE: &str = "work.css";
/// Work relationship graph stylesheet.
pub const WORK_MAP_STYLE: &str = "work-map.css";
/// Responsive-layout stylesheet.
pub const RESPONSIVE_STYLE: &str = "responsive.css";
/// Browser application WebAssembly module.
pub const APP_WASM: &str = "app_bg.wasm";
/// The sole current multi-target release manifest schema.
pub const RELEASE_MANIFEST_VERSION: u16 = 1;
/// The sole current deterministic release-note document schema.
pub const RELEASE_NOTES_VERSION: u16 = 1;
/// The sole current externally installable release-package schema.
pub const RELEASE_PACKAGE_VERSION: u16 = 1;

/// An OCI image identity that is pinned by one canonical SHA-256 digest.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProofImage(String);

impl ProofImage {
    /// Exact admitted OCI identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this text is one admitted digest-pinned OCI identity.
    #[must_use]
    pub fn admits(text: &str) -> bool {
        let digest = if let Some(digest) = text.strip_prefix("sha256:") {
            Some(digest)
        } else if let Some((repository, digest)) = text.rsplit_once("@sha256:") {
            canonical_repository(repository).then_some(digest)
        } else {
            None
        };
        digest.is_some_and(|digest| {
            digest.len() == OCI_SHA256_HEX_LEN
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }
}

fn canonical_repository(repository: &str) -> bool {
    let components = repository.split('/').collect::<Vec<_>>();
    if components.iter().any(|component| component.is_empty()) {
        return false;
    }
    let path_start = if components.len() > 1
        && (components[0].contains(['.', ':']) || components[0] == "localhost")
    {
        if !canonical_registry(components[0]) {
            return false;
        }
        1
    } else {
        0
    };
    components[path_start..]
        .iter()
        .all(|component| canonical_repository_component(component))
}

fn canonical_registry(registry: &str) -> bool {
    let (host, port) = match registry.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (registry, None),
    };
    if port.is_some_and(|port| port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit())) {
        return false;
    }
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn canonical_repository_component(component: &str) -> bool {
    let bytes = component.as_bytes();
    if bytes.is_empty()
        || !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit()
        || !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit()
    {
        return false;
    }
    let mut previous_separator = false;
    for &byte in bytes {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            previous_separator = false;
        } else if matches!(byte, b'.' | b'_' | b'-') && !previous_separator {
            previous_separator = true;
        } else {
            return false;
        }
    }
    true
}

/// Why an OCI proof image identity was refused.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("proof image must be a canonical lower-case sha256 digest reference: {0:?}")]
pub struct ProofImageError(String);

impl FromStr for ProofImage {
    type Err = ProofImageError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if Self::admits(text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(ProofImageError(text.to_owned()))
        }
    }
}

impl fmt::Display for ProofImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ProofImage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ProofImage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

/// Exact immutable image selected for one hosted Windows execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostedWindowsImage {
    pub label: String,
    pub provider_os: String,
    pub version: String,
    pub os_version: String,
    pub architecture: String,
}

/// Exact boundary toolchain selected inside one hosted Windows image.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostedWindowsToolchain {
    pub rust: String,
    pub cargo: String,
    pub git: String,
    pub trunk: String,
    pub msvc: String,
    pub sdk: String,
    pub rustc_path: String,
    pub cargo_path: String,
    pub git_path: String,
    pub trunk_path: String,
    pub cl_path: String,
    pub rc_path: String,
    pub link_path: String,
    pub path: String,
    pub include: String,
    pub lib: String,
}

/// One prebuilt executable a hosted Windows run verified byte for byte before
/// running it: its path in the cross-built bundle and the lowercase hex
/// SHA-256 of its exact bytes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostedWindowsExecutable {
    pub path: String,
    pub sha256: String,
}

/// Provider-authenticated identity of one ephemeral Windows environment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostedWindowsIdentity {
    pub provider: String,
    pub repository: String,
    pub workflow: String,
    pub run_id: u64,
    pub run_attempt: u32,
    pub image: HostedWindowsImage,
    pub runner: Digest,
    pub source: String,
    /// The native toolchain the run observed. A run that executed only the
    /// cross-built bundle's prebuilt executables admits no native toolchain
    /// and carries none; every other run encodes exactly as it did before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolchain: Option<HostedWindowsToolchain>,
    /// Every bundle executable the run verified, in request order. Runs that
    /// executed only admitted tools carry none, and encode exactly as they did
    /// before bundle executables existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executables: Vec<HostedWindowsExecutable>,
}

/// The one sealed execution identity carried by replay evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum ProofEnvironment {
    /// A digest-pinned OCI image executed through the sealed container route.
    Oci { image: ProofImage },
    /// The domain-separated digest of one admitted native runner executable.
    Native { runner: Digest },
    /// One provider-authenticated ephemeral native Windows execution; its
    /// identity is boxed because it is many times the size of the other two.
    #[serde(rename = "hosted-windows")]
    HostedWindows {
        identity: Box<HostedWindowsIdentity>,
    },
}

impl ProofEnvironment {
    /// Construct an OCI identity from its strict wire spelling.
    ///
    /// # Errors
    ///
    /// Refuses tags, upper-case digests, and noncanonical repository spellings.
    pub fn oci(image: impl AsRef<str>) -> Result<Self, ProofImageError> {
        Ok(Self::Oci {
            image: image.as_ref().parse()?,
        })
    }

    /// Construct a native identity from observed runner bytes.
    #[must_use]
    pub const fn native(runner: Digest) -> Self {
        Self::Native { runner }
    }

    /// Construct an ephemeral hosted Windows identity.
    #[must_use]
    pub fn hosted_windows(identity: HostedWindowsIdentity) -> Self {
        Self::HostedWindows {
            identity: Box::new(identity),
        }
    }
}

/// A release tuple with a distinct artifact identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum ReleaseTarget {
    #[serde(rename = "aarch64-apple-darwin")]
    Aarch64AppleDarwin,
    #[serde(rename = "x86_64-unknown-linux-musl")]
    X86_64UnknownLinuxMusl,
    #[serde(rename = "x86_64-pc-windows-msvc")]
    X86_64PcWindowsMsvc,
}

impl ReleaseTarget {
    /// The canonical Rust target-triple spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aarch64AppleDarwin => "aarch64-apple-darwin",
            Self::X86_64UnknownLinuxMusl => "x86_64-unknown-linux-musl",
            Self::X86_64PcWindowsMsvc => "x86_64-pc-windows-msvc",
        }
    }

    /// Canonical path of this target's executable within its target stage.
    #[must_use]
    pub const fn executable_path(self) -> &'static str {
        match self {
            Self::Aarch64AppleDarwin | Self::X86_64UnknownLinuxMusl => "bin/spice",
            Self::X86_64PcWindowsMsvc => "bin/spice.exe",
        }
    }

    /// Canonical path of this target's separately packaged debug artifact.
    #[must_use]
    pub const fn debug_path(self) -> Option<&'static str> {
        match self {
            Self::Aarch64AppleDarwin => Some("symbols/spice.symbols"),
            Self::X86_64UnknownLinuxMusl => None,
            Self::X86_64PcWindowsMsvc => Some("symbols/spice.pdb"),
        }
    }
}

impl fmt::Display for ReleaseTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One canonical semantic product version without a Git tag prefix.
///
/// Equality is textual. Order is deliberately not derived: SemVer precedence
/// ignores build metadata, so it cannot be a total order consistent with
/// equality. Compare with [`Self::precedence`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductVersion(Box<AdmittedVersion>);

/// Boxed so errors and records carrying a version stay pointer-sized.
#[derive(Clone, Debug, Eq, PartialEq)]
struct AdmittedVersion {
    /// The admitted spelling, kept verbatim.
    text: String,
    version: semver::Version,
}

impl ProductVersion {
    /// Admit one canonical semantic version.
    ///
    /// # Errors
    ///
    /// Refuses whatever `semver` refuses: whitespace, a `v` prefix, missing
    /// core numbers, leading zeroes, and malformed pre-release or build
    /// identifiers.
    pub fn parse(text: &str) -> Result<Self, ProductVersionError> {
        semver::Version::parse(text)
            .map(|version| {
                Self(Box::new(AdmittedVersion {
                    text: text.to_owned(),
                    version,
                }))
            })
            .map_err(|_| ProductVersionError(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0.text
    }

    /// SemVer 2.0.0 precedence: numeric core, then pre-release identifiers,
    /// with build metadata ignored.
    #[must_use]
    pub fn precedence(&self, other: &Self) -> Ordering {
        self.0.version.cmp_precedence(&other.0.version)
    }
}

impl FromStr for ProductVersion {
    type Err = ProductVersionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl fmt::Display for ProductVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ProductVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ProductVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

/// Why one external product version was refused.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("product version is not canonical semantic version text: {0:?}")]
pub struct ProductVersionError(String);

/// The rack member `build.json` every appliance build links: the product
/// version the executable reports and the source commit it was built from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildInfo {
    pub product: ProductVersion,
    /// Absent for a development build bound to no commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<BuildSource>,
}

/// The exact commit a build was made from and its committer second.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSource {
    pub commit: String,
    pub committed: i64,
}

/// Exact identities required before distribution may build.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildPins {
    pub rust: String,
    pub wasm_target: String,
    pub trunk: String,
    pub proof_container: String,
}

/// One staged file bound into a target payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryEntry {
    pub path: String,
    pub mode: u32,
    pub bytes: u64,
    pub digest: Digest,
}

/// One deterministic dSYM payload bound to its stripped executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MacSymbolArtifact {
    pub version: u16,
    pub uuid: String,
    pub executable: Digest,
    /// Full lowercase hex Git commit the executable was linked from.
    pub commit: String,
    /// Whether tracked files differed from that commit when linked.
    pub dirty: bool,
    pub info: Vec<u8>,
    pub dwarf: Vec<u8>,
}

impl MacSymbolArtifact {
    /// Encode the sole current canonical artifact: its metadata header, then
    /// one deterministic deflate stream over both members.
    ///
    /// # Errors
    ///
    /// Refuses a stale schema or a raw or encoded size above its fixed bound.
    pub fn encode(&self) -> Result<Vec<u8>, MacSymbolArtifactError> {
        self.require_current_and_bounded()?;
        let mut encoded = BoundedBytes::new(MAC_SYMBOL_ARTIFACT_MAX_BYTES as usize);
        let result = write_mac_symbol_artifact(self, &mut encoded);
        if encoded.exceeded {
            return Err(MacSymbolArtifactError::EncodedSize {
                observed: MAC_SYMBOL_ARTIFACT_MAX_BYTES + 1,
            });
        }
        result.map_err(MacSymbolArtifactError::Encode)?;
        Ok(encoded.bytes)
    }

    /// Decode and admit only the sole current canonical artifact.
    ///
    /// # Errors
    ///
    /// Refuses oversize, malformed, noncanonical, corrupt, and non-current
    /// artifacts before exposing their members.
    pub fn decode(bytes: &[u8]) -> Result<Self, MacSymbolArtifactError> {
        if !mac_symbol_encoded_bytes_admitted(bytes.len() as u64) {
            return Err(MacSymbolArtifactError::EncodedSize {
                observed: bytes.len() as u64,
            });
        }
        let version = mac_symbol_artifact_version(bytes)?;
        if version != MAC_SYMBOL_ARTIFACT_VERSION {
            return Err(MacSymbolArtifactError::Version { version });
        }
        let (header, compressed) = current_mac_symbol_parts(bytes)?;
        let encoded: EncodedMacSymbolArtifact<'_> = serde_json::from_slice(header)
            .map_err(|error| MacSymbolArtifactError::Decode(error.to_string()))?;
        if encoded.version != MAC_SYMBOL_ARTIFACT_VERSION {
            return Err(MacSymbolArtifactError::Version {
                version: encoded.version,
            });
        }
        if !mac_symbol_raw_bytes_admitted(encoded.info_bytes, encoded.dwarf_bytes) {
            return Err(MacSymbolArtifactError::RawSize);
        }
        let info_bytes =
            usize::try_from(encoded.info_bytes).map_err(|_| MacSymbolArtifactError::RawSize)?;
        let raw_bytes = encoded
            .info_bytes
            .checked_add(encoded.dwarf_bytes)
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or(MacSymbolArtifactError::RawSize)?;
        let decoder = DeflateDecoder::new(compressed);
        let mut limited = decoder.take((raw_bytes as u64).saturating_add(1));
        let mut raw = Vec::new();
        limited
            .read_to_end(&mut raw)
            .map_err(|error| MacSymbolArtifactError::Decode(error.to_string()))?;
        if raw.len() != raw_bytes {
            return Err(MacSymbolArtifactError::Decode(format!(
                "decoded {} bytes, expected {raw_bytes}",
                raw.len()
            )));
        }
        let dwarf = raw.split_off(info_bytes);
        let artifact = Self {
            version: encoded.version,
            uuid: encoded.uuid.to_owned(),
            executable: encoded.executable,
            commit: encoded.commit.to_owned(),
            dirty: encoded.dirty,
            info: raw,
            dwarf,
        };
        artifact.require_current_and_bounded()?;
        let mut canonical = CanonicalBytes::new(bytes);
        write_mac_symbol_artifact(&artifact, &mut canonical)
            .map_err(MacSymbolArtifactError::Encode)?;
        if !canonical.matches() {
            return Err(MacSymbolArtifactError::NonCanonical);
        }
        Ok(artifact)
    }

    fn require_current_and_bounded(&self) -> Result<(), MacSymbolArtifactError> {
        if self.version != MAC_SYMBOL_ARTIFACT_VERSION {
            return Err(MacSymbolArtifactError::Version {
                version: self.version,
            });
        }
        if !mac_symbol_raw_bytes_admitted(self.info.len() as u64, self.dwarf.len() as u64) {
            return Err(MacSymbolArtifactError::RawSize);
        }
        Ok(())
    }

    /// Whether the UUID has the canonical uppercase Mach-O spelling.
    #[must_use]
    pub fn uuid_is_canonical(&self) -> bool {
        macho_uuid_is_canonical(&self.uuid)
    }

    /// Whether the commit is a full lowercase SHA-1 or SHA-256 object name.
    #[must_use]
    pub fn commit_is_canonical(&self) -> bool {
        git_commit_is_canonical(&self.commit)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EncodedMacSymbolArtifact<'a> {
    version: u16,
    uuid: &'a str,
    executable: Digest,
    commit: &'a str,
    dirty: bool,
    info_bytes: u64,
    dwarf_bytes: u64,
}

/// Why a macOS symbol artifact did not cross its current schema boundary.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MacSymbolArtifactError {
    #[error("macOS symbol artifact header is not canonical")]
    Header,
    #[error("macOS symbol artifact version {version} is not current")]
    Version { version: u16 },
    #[error("macOS symbol artifact decoded members exceed their fixed byte limit")]
    RawSize,
    #[error(
        "macOS symbol artifact has at least {observed} encoded bytes, exceeding its fixed byte limit"
    )]
    EncodedSize { observed: u64 },
    #[error("macOS symbol artifact does not decode: {0}")]
    Decode(String),
    #[error("macOS symbol artifact does not encode: {0}")]
    Encode(String),
    #[error("macOS symbol artifact bytes are not canonical")]
    NonCanonical,
}

fn mac_symbol_artifact_version(bytes: &[u8]) -> Result<u16, MacSymbolArtifactError> {
    if bytes.starts_with(MAC_SYMBOL_MAGIC) {
        return Ok(MAC_SYMBOL_ARTIFACT_VERSION);
    }
    bytes
        .strip_prefix(b"{\"version\":")
        .and_then(|rest| rest.split(|byte| *byte == b',').next())
        .filter(|version| {
            !version.is_empty()
                && (version.len() == 1 || version[0] != b'0')
                && version.iter().all(u8::is_ascii_digit)
        })
        .and_then(|version| std::str::from_utf8(version).ok())
        .and_then(|version| version.parse().ok())
        .ok_or(MacSymbolArtifactError::Header)
}

fn current_mac_symbol_parts(bytes: &[u8]) -> Result<(&[u8], &[u8]), MacSymbolArtifactError> {
    let body = bytes
        .strip_prefix(MAC_SYMBOL_MAGIC)
        .ok_or(MacSymbolArtifactError::Header)?;
    let header_bytes = body
        .iter()
        .take(MAC_SYMBOL_HEADER_MAX_BYTES + 1)
        .position(|byte| *byte == b'\n')
        .filter(|length| *length <= MAC_SYMBOL_HEADER_MAX_BYTES)
        .ok_or(MacSymbolArtifactError::Header)?;
    Ok((&body[..header_bytes], &body[header_bytes + 1..]))
}

const fn mac_symbol_encoded_bytes_admitted(bytes: u64) -> bool {
    bytes <= MAC_SYMBOL_ARTIFACT_MAX_BYTES
}

const fn mac_symbol_raw_bytes_admitted(info: u64, dwarf: u64) -> bool {
    info <= MAC_SYMBOL_MEMBER_BYTES_LIMIT
        && dwarf <= MAC_SYMBOL_MEMBER_BYTES_LIMIT
        && matches!(
            info.checked_add(dwarf),
            Some(bytes) if bytes <= MAC_SYMBOL_RAW_BYTES_LIMIT
        )
}

fn write_mac_symbol_artifact(
    artifact: &MacSymbolArtifact,
    writer: &mut impl Write,
) -> Result<(), String> {
    writer
        .write_all(MAC_SYMBOL_MAGIC)
        .map_err(|error| error.to_string())?;
    write!(writer, "{{\"version\":{},\"uuid\":", artifact.version)
        .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut *writer, &artifact.uuid).map_err(|error| error.to_string())?;
    writer
        .write_all(b",\"executable\":")
        .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut *writer, &artifact.executable).map_err(|error| error.to_string())?;
    writer
        .write_all(b",\"commit\":")
        .map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut *writer, &artifact.commit).map_err(|error| error.to_string())?;
    writeln!(
        writer,
        ",\"dirty\":{},\"info_bytes\":{},\"dwarf_bytes\":{}}}",
        if artifact.dirty { "true" } else { "false" },
        artifact.info.len(),
        artifact.dwarf.len()
    )
    .map_err(|error| error.to_string())?;
    let mut deflate = DeflateEncoder::new(&mut *writer, Compression::new(MAC_SYMBOL_DEFLATE_LEVEL));
    deflate
        .write_all(&artifact.info)
        .and_then(|()| deflate.write_all(&artifact.dwarf))
        .and_then(|()| deflate.finish().map(|_| ()))
        .map_err(|error| error.to_string())
}

struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedBytes {
    const fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > self.limit)
        {
            self.exceeded = true;
            return Err(io::Error::other("encoded symbol artifact size limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct CanonicalBytes<'a> {
    expected: &'a [u8],
    offset: usize,
    matches: bool,
}

impl<'a> CanonicalBytes<'a> {
    const fn new(expected: &'a [u8]) -> Self {
        Self {
            expected,
            offset: 0,
            matches: true,
        }
    }

    fn matches(&self) -> bool {
        self.matches && self.offset == self.expected.len()
    }
}

impl io::Write for CanonicalBytes<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self.offset.saturating_add(bytes.len());
        if end > self.expected.len() || self.expected.get(self.offset..end) != Some(bytes) {
            self.matches = false;
        }
        self.offset = end;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Whether `uuid` has the canonical uppercase Mach-O spelling.
#[must_use]
pub fn macho_uuid_is_canonical(uuid: &str) -> bool {
    uuid.len() == MACHO_UUID_LENGTH
        && uuid.bytes().enumerate().all(|(index, byte)| {
            if MACHO_UUID_HYPHENS.contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte)
            }
        })
}

/// Whether `commit` is a full lowercase SHA-1 or SHA-256 Git object name.
#[must_use]
pub fn git_commit_is_canonical(commit: &str) -> bool {
    matches!(commit.len(), 40 | 64)
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Authenticated identity of one platform-conditional macOS symbol artifact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MacSymbolMetadata {
    pub path: String,
    pub mode: u32,
    pub bytes: u64,
    pub digest: Digest,
    pub uuid: String,
    pub executable: Digest,
}

impl MacSymbolMetadata {
    /// Whether the UUID has the canonical uppercase Mach-O spelling.
    #[must_use]
    pub fn uuid_is_canonical(&self) -> bool {
        macho_uuid_is_canonical(&self.uuid)
    }
}

/// The receipt-free object submitted to proof for one release target.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPayload {
    pub version: u16,
    pub target: ReleaseTarget,
    /// Domain-separated identity of the immutable Git commit built.
    pub source: Digest,
    /// Identity of the exact source archive bytes the target was built from.
    pub archive: Digest,
    /// Identity of the complete immutable app generation the executable links.
    pub app: Digest,
    /// Identity of the exact canvas archive bytes linked into the executable.
    pub canvas: Digest,
    pub native: Digest,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub symbols: Option<MacSymbolMetadata>,
    pub pins: BuildPins,
    pub pins_digest: Digest,
    pub inventory: Vec<InventoryEntry>,
    pub inventory_digest: Digest,
}

/// Whether the complete sealed proof battery passed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProofStanding {
    /// Every declared battery member completed and its evidence was whole.
    Clean,
    /// At least one battery member failed, timed out, or produced incomplete evidence.
    Refused,
}

/// Detached evidence binding one sealed replay to one receipt-free target payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProofReceipt {
    /// Current detached-receipt schema.
    pub version: u16,
    /// Release tuple the payload and replay both cover.
    pub target: ReleaseTarget,
    /// Canonical identity of the receipt-free target payload.
    pub payload: Digest,
    /// Canonical identity of the ordered battery command inventory.
    pub battery: Digest,
    /// Canonical identity of the complete typed replay result.
    pub replay: Digest,
    /// Digest-pinned container or native runner identity that executed the replay.
    pub environment: ProofEnvironment,
    /// One standing for the complete battery.
    pub standing: ProofStanding,
}

/// One proof-admitted target in the complete release manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifestTarget {
    /// Release tuple carried by both the payload and receipt.
    pub target: ReleaseTarget,
    /// Canonical receipt-free target-payload identity.
    pub payload: Digest,
    /// Exact encoded length of the target-payload object.
    pub payload_bytes: u64,
    /// Complete canonical artifact inventory authenticated by the payload.
    pub inventory: Vec<InventoryEntry>,
    /// Platform-conditional macOS symbol identity authenticated by the payload.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub symbols: Option<MacSymbolMetadata>,
    /// Canonical detached proof-receipt identity.
    pub receipt: Digest,
    /// Exact encoded length of the detached proof-receipt object.
    pub receipt_bytes: u64,
    /// Current generated proof battery identity.
    pub battery: Digest,
    /// Complete replay-result identity.
    pub replay: Digest,
    /// Exact sealed environment that executed the replay.
    pub environment: ProofEnvironment,
    /// Explicit clean standing verified before this entry was constructed.
    pub standing: ProofStanding,
}

/// The canonical complete release assembled from all supported targets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    /// Current release-manifest schema.
    pub version: u16,
    /// One exact source identity shared by every target payload.
    pub source: Digest,
    /// One exact source archive every target payload was built from.
    pub archive: Digest,
    /// All supported targets in [`ReleaseTarget`] declaration order.
    pub targets: Vec<ReleaseManifestTarget>,
}
/// One externally installable executable and its sealed provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasePackageTarget {
    /// Supported target tuple in declaration order.
    pub target: ReleaseTarget,
    /// Fixed package-tree path of the standalone executable.
    pub executable_path: String,
    /// Domain-separated identity of the exact executable bytes.
    pub executable: Digest,
    /// Canonical receipt-free target payload identity.
    pub payload: Digest,
    /// Detached proof receipt that admitted the payload.
    pub proof: Digest,
    /// Fixed package-tree path of the target's debug artifact, when present.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub debug_path: Option<String>,
    /// Fixed package-tree path of the detached signed proof receipt.
    pub signature_path: String,
}

/// Canonical index of one externally installable Spice release.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasePackage {
    /// Current package-index schema.
    pub schema: u16,
    /// Stable external product name.
    pub package: String,
    /// Canonical semantic product version without a tag prefix.
    pub product_version: ProductVersion,
    /// Immutable source commit identity shared by every target payload.
    pub source: Digest,
    /// Canonical sealed all-target manifest identity.
    pub manifest: Digest,
    /// Identity of the exact release-note evidence document.
    pub notes: Digest,
    /// Identity of the exact operator-facing note bytes in the package tree.
    pub notes_content: Digest,
    /// All supported targets in [`ReleaseTarget`] declaration order.
    pub targets: Vec<ReleasePackageTarget>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Domain;

    fn symbol_artifact() -> MacSymbolArtifact {
        MacSymbolArtifact {
            version: MAC_SYMBOL_ARTIFACT_VERSION,
            uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
            executable: Digest::compute(Domain::DistArtifact, b"executable"),
            commit: "1".repeat(40),
            dirty: false,
            info: b"fixture plist".to_vec(),
            dwarf: b"fixture DWARF".to_vec(),
        }
    }

    #[test]
    fn mac_symbol_artifact_compresses_and_round_trips_complete_members() {
        let mut artifact = symbol_artifact();
        artifact.dwarf = vec![0x5A; 64 * 1024];
        let raw_bytes = artifact.info.len() + artifact.dwarf.len();
        let encoded = artifact.encode().expect("artifact encodes");
        assert!(encoded.len() < raw_bytes / 8);
        assert_eq!(
            MacSymbolArtifact::decode(&encoded).expect("artifact decodes"),
            artifact
        );
    }

    #[test]
    fn non_current_schema_refuses_before_current_decode() {
        let retired = b"{\"version\":3,\"retired\":true}";
        assert_eq!(
            MacSymbolArtifact::decode(retired),
            Err(MacSymbolArtifactError::Version { version: 3 })
        );
    }

    #[test]
    fn mac_symbol_artifact_refuses_corrupt_compressed_data() {
        let mut corrupted = symbol_artifact().encode().expect("artifact encodes");
        let data = corrupted
            .iter()
            .rposition(|byte| *byte == b'\n')
            .expect("artifact separates its compressed data")
            + 1;
        corrupted[data] = corrupted[data].wrapping_add(1);
        assert!(MacSymbolArtifact::decode(&corrupted).is_err());
    }

    #[test]
    fn mac_symbol_artifact_refuses_truncated_and_trailing_compressed_data() {
        let encoded = symbol_artifact().encode().expect("artifact encodes");
        let mut truncated = encoded.clone();
        truncated.pop();
        assert!(MacSymbolArtifact::decode(&truncated).is_err());

        let mut trailing = encoded;
        trailing.push(0);
        assert!(MacSymbolArtifact::decode(&trailing).is_err());
    }

    #[test]
    fn mac_symbol_artifact_bounds_decompression_by_declared_lengths() {
        let mut encoded = symbol_artifact().encode().expect("artifact encodes");
        let declared = b"\"dwarf_bytes\":13";
        let at = encoded
            .windows(declared.len())
            .position(|window| window == declared)
            .expect("fixture declares its DWARF length");
        encoded[at + declared.len() - 1] = b'2';
        assert!(matches!(
            MacSymbolArtifact::decode(&encoded),
            Err(MacSymbolArtifactError::Decode(_))
        ));
    }

    #[test]
    fn mac_symbol_decoded_size_bound_admits_its_edge_only() {
        assert!(mac_symbol_raw_bytes_admitted(0, MAC_SYMBOL_RAW_BYTES_LIMIT));
        assert!(!mac_symbol_raw_bytes_admitted(
            0,
            MAC_SYMBOL_RAW_BYTES_LIMIT + 1
        ));
        assert!(!mac_symbol_raw_bytes_admitted(
            MAC_SYMBOL_RAW_BYTES_LIMIT / 2 + 1,
            MAC_SYMBOL_RAW_BYTES_LIMIT - MAC_SYMBOL_RAW_BYTES_LIMIT / 2,
        ));
    }

    #[test]
    fn mac_symbol_encoded_size_bound_admits_its_edge_only() {
        assert!(mac_symbol_encoded_bytes_admitted(
            MAC_SYMBOL_ARTIFACT_MAX_BYTES
        ));
        assert!(!mac_symbol_encoded_bytes_admitted(
            MAC_SYMBOL_ARTIFACT_MAX_BYTES + 1
        ));
    }
}
