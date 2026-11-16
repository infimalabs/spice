//! Typed request, result, and admission contract for one disposable hosted Windows execution.

pub mod client;
pub mod release_inputs;

use std::error::Error;
use std::fmt;

use crate::protocol::{
    Command, CommandResult, Limits, MAX_ARGUMENTS, MAX_OUTPUT_BYTES, MAX_RUNTIME_SECONDS,
};
use serde::{Deserialize, Serialize};
use wire::Digest;
pub use wire::manifest::{
    HostedWindowsIdentity as Identity, HostedWindowsImage as ImagePin,
    HostedWindowsToolchain as ToolchainPins,
};

/// The sole current hosted Windows execution protocol.
pub const PROTOCOL_VERSION: u16 = 3;
/// Provider spelling admitted by the current protocol.
pub const GITHUB_PROVIDER: &str = "github-actions";
/// Architecture spelling emitted by GitHub's Windows runner.
pub const X86_64_ARCHITECTURE: &str = "X64";
/// Repository-owned workflow that is allowed to sign a result.
pub const WORKFLOW: &str = ".github/workflows/windows.yml";
/// Name prefix of the provider artifact carrying a staged Linux canvas; the
/// canvas digest completes the name.
pub const CANVAS_ARTIFACT_PREFIX: &str = "canvas-";

const DIGEST_HEX_LENGTH: usize = 64;
const MAX_COMMANDS: usize = 8;
const MAX_ARTIFACTS: usize = 256;
const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;

/// Complete client request signed into one provider run.
///
/// `source` is the canonical archive identity; `archive_sha256` is the raw
/// transported `source.tar` digest; `source_commit` is the canonical commit
/// recorded inside that archive; and `tree` is its exact file listing. A
/// release stage's `canvas` names the staged Linux canvas the guest links
/// instead of compiling the app.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub challenge: String,
    pub provider: String,
    pub repository: String,
    pub workflow_ref: String,
    pub source_commit: String,
    pub source: String,
    pub archive_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_asset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_inputs: Option<ReleaseInputsPin>,
    pub tree: String,
    pub image: ImagePin,
    pub toolchain: ToolchainPins,
    pub limits: Limits,
    pub commands: Vec<Command>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas: Option<CanvasPin>,
}

/// The staged Linux canvas one release stage adopts: the provider run whose
/// `canvas-DIGEST` artifact carries it, and the canvas digest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasPin {
    pub run_id: u64,
    pub digest: Digest,
}

impl CanvasPin {
    /// The provider artifact name carrying this canvas.
    #[must_use]
    pub fn artifact(&self) -> String {
        format!("{CANVAS_ARTIFACT_PREFIX}{}", self.digest)
    }
}
/// Provider-produced release inputs consumed unchanged by one native stage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseInputsPin {
    pub run_id: u64,
    pub challenge: String,
    pub app: Digest,
    pub canvas: Digest,
    pub proof_identity: String,
    pub workflow_sha: String,
}
impl ReleaseInputsPin {
    fn admit(&self) -> Result<(), AdmissionError> {
        if self.run_id == 0 {
            return Err(AdmissionError::Canvas);
        }
        require_hex(
            "release input challenge",
            &self.challenge,
            DIGEST_HEX_LENGTH,
        )?;
        require_hex("release input workflow", &self.workflow_sha, 40)?;
        let proof = self
            .proof_identity
            .strip_prefix("sha256:")
            .ok_or(AdmissionError::Digest("release input proof identity"))?;
        require_hex("release input proof identity", proof, DIGEST_HEX_LENGTH)
    }
}

impl Request {
    /// Refuse malformed or unbounded input before any provider effect.
    pub fn admit(&self) -> Result<(), AdmissionError> {
        self.admit_identity()?;
        self.admit_image()?;
        self.admit_limits()?;
        self.admit_commands()?;
        self.admit_toolchain()
    }

    fn admit_identity(&self) -> Result<(), AdmissionError> {
        if self.version != PROTOCOL_VERSION {
            return Err(AdmissionError::Protocol(self.version));
        }
        if self.provider != GITHUB_PROVIDER {
            return Err(AdmissionError::Provider(self.provider.clone()));
        }
        require_hex("challenge", &self.challenge, DIGEST_HEX_LENGTH)?;
        require_hex("source_commit", &self.source_commit, 40)?;
        require_hex("source", &self.source, DIGEST_HEX_LENGTH)?;
        require_hex("archive_sha256", &self.archive_sha256, DIGEST_HEX_LENGTH)?;
        require_hex("tree", &self.tree, DIGEST_HEX_LENGTH)?;
        if self.repository.split('/').count() != 2
            || self.repository.starts_with('/')
            || self.repository.ends_with('/')
        {
            return Err(AdmissionError::Repository(self.repository.clone()));
        }
        if self.workflow_ref != "refs/heads/main" {
            return Err(AdmissionError::WorkflowRef(self.workflow_ref.clone()));
        }
        match (&self.release_inputs, self.transport_asset) {
            (Some(inputs), None) => inputs.admit()?,
            (None, Some(asset)) if asset != 0 => {}
            _ => return Err(AdmissionError::TransportAsset),
        }
        Ok(())
    }

    fn admit_image(&self) -> Result<(), AdmissionError> {
        if self.image.architecture != X86_64_ARCHITECTURE {
            return Err(AdmissionError::Architecture(
                self.image.architecture.clone(),
            ));
        }
        if self.image.label.is_empty()
            || self.image.provider_os.is_empty()
            || self.image.version.is_empty()
            || self.image.os_version.is_empty()
        {
            return Err(AdmissionError::Image);
        }
        Ok(())
    }

    fn admit_limits(&self) -> Result<(), AdmissionError> {
        if self.limits.runtime_seconds == 0 || self.limits.runtime_seconds > MAX_RUNTIME_SECONDS {
            return Err(AdmissionError::Runtime(self.limits.runtime_seconds));
        }
        if self.limits.output_bytes == 0 || self.limits.output_bytes > MAX_OUTPUT_BYTES {
            return Err(AdmissionError::Output(self.limits.output_bytes));
        }
        Ok(())
    }

    fn admit_commands(&self) -> Result<(), AdmissionError> {
        if self.commands.is_empty() || self.commands.len() > MAX_COMMANDS {
            return Err(AdmissionError::Commands);
        }
        for command in &self.commands {
            if command.program.is_empty() || command.program.contains('\0') {
                return Err(AdmissionError::Program);
            }
            if command.arguments.len() > MAX_ARGUMENTS
                || command
                    .arguments
                    .iter()
                    .any(|argument| argument.contains('\0'))
            {
                return Err(AdmissionError::Arguments);
            }
        }
        if self
            .canvas
            .as_ref()
            .is_some_and(|canvas| canvas.run_id == 0)
        {
            return Err(AdmissionError::Canvas);
        }
        Ok(())
    }

    fn admit_toolchain(&self) -> Result<(), AdmissionError> {
        for (name, value) in [
            ("rust", self.toolchain.rust.as_str()),
            ("cargo", self.toolchain.cargo.as_str()),
            ("git", self.toolchain.git.as_str()),
            ("trunk", self.toolchain.trunk.as_str()),
            ("msvc", self.toolchain.msvc.as_str()),
            ("sdk", self.toolchain.sdk.as_str()),
            ("rustc_path", self.toolchain.rustc_path.as_str()),
            ("cargo_path", self.toolchain.cargo_path.as_str()),
            ("git_path", self.toolchain.git_path.as_str()),
            ("trunk_path", self.toolchain.trunk_path.as_str()),
            ("cl_path", self.toolchain.cl_path.as_str()),
            ("rc_path", self.toolchain.rc_path.as_str()),
            ("link_path", self.toolchain.link_path.as_str()),
            ("path", self.toolchain.path.as_str()),
            ("include", self.toolchain.include.as_str()),
            ("lib", self.toolchain.lib.as_str()),
        ] {
            if value.is_empty() || value.contains('\0') {
                return Err(AdmissionError::Toolchain(name));
            }
        }
        Ok(())
    }
}

/// One provider-signed output file returned by a hosted build.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: String,
    pub bytes: u64,
    pub digest: Digest,
}

/// Complete signed output of one hosted execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultDocument {
    pub version: u16,
    pub challenge: String,
    pub request: Request,
    pub identity: Identity,
    pub outputs: Vec<CommandResult>,
    pub artifacts: Vec<Artifact>,
    pub isolation: Isolation,
}

/// Isolation facts established before the command and torn down afterward.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Isolation {
    pub source_read_only: bool,
    pub fresh_cargo_home: bool,
    pub fresh_target: bool,
    pub loopback_only: bool,
    pub process_tree_bounded: bool,
}

/// Admit a provider-verified result against the exact one-shot request and
/// independently attested runner executable.
pub fn admit_result(
    request: &Request,
    result: &ResultDocument,
    runner: &Digest,
) -> Result<Identity, AdmissionError> {
    request.admit()?;
    if result.version != PROTOCOL_VERSION {
        return Err(AdmissionError::Protocol(result.version));
    }
    if result.challenge != request.challenge {
        return Err(AdmissionError::Challenge);
    }
    if &result.request != request {
        return Err(AdmissionError::Request);
    }
    let mut identity = result.identity.clone();
    if identity.provider != request.provider {
        return Err(AdmissionError::Provider(identity.provider.clone()));
    }
    if identity.repository != request.repository || identity.workflow != WORKFLOW {
        return Err(AdmissionError::Workload);
    }
    if identity.run_id == 0 || identity.run_attempt == 0 {
        return Err(AdmissionError::RunIdentity);
    }
    if !image_admits(&request.image, &mut identity.image) {
        return Err(AdmissionError::Image);
    }
    if identity.source != request.source {
        return Err(AdmissionError::Source);
    }
    if !toolchain_admits(&request.toolchain, &mut identity.toolchain) {
        return Err(AdmissionError::Toolchain("observed"));
    }
    if &identity.runner != runner {
        return Err(AdmissionError::Runner);
    }
    if !result.isolation.complete() {
        return Err(AdmissionError::Isolation);
    }
    admit_outputs(request, &result.outputs)?;
    admit_artifacts(&result.artifacts)?;
    Ok(identity)
}

fn image_admits(expected: &ImagePin, observed: &mut ImagePin) -> bool {
    if observed.version.is_empty() {
        return false;
    }
    let version = std::mem::replace(&mut observed.version, expected.version.clone());
    let admits = observed == expected;
    observed.version = version;
    admits
}

fn toolchain_admits(expected: &ToolchainPins, observed: &mut ToolchainPins) -> bool {
    if observed.git.is_empty() {
        return false;
    }
    let git = std::mem::replace(&mut observed.git, expected.git.clone());
    let admits = observed == expected;
    observed.git = git;
    admits
}

impl Isolation {
    fn complete(&self) -> bool {
        self.source_read_only
            && self.fresh_cargo_home
            && self.fresh_target
            && self.loopback_only
            && self.process_tree_bounded
    }
}

fn admit_outputs(request: &Request, outputs: &[CommandResult]) -> Result<(), AdmissionError> {
    if outputs.len() != request.commands.len() {
        return Err(AdmissionError::Commands);
    }
    for output in outputs {
        let stdout = output.stdout.bytes().map_err(|_| AdmissionError::Capture)?;
        let stderr = output.stderr.bytes().map_err(|_| AdmissionError::Capture)?;
        if stdout
            .len()
            .checked_add(stderr.len())
            .is_none_or(|bytes| bytes > request.limits.output_bytes as usize)
        {
            return Err(AdmissionError::Capture);
        }
    }
    Ok(())
}

fn admit_artifacts(artifacts: &[Artifact]) -> Result<(), AdmissionError> {
    let mut previous = None;
    let mut artifact_bytes = 0_u64;
    for artifact in artifacts {
        if artifact.path.is_empty()
            || artifact.path.contains('\\')
            || artifact
                .path
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
            || previous.is_some_and(|path| path >= artifact.path.as_str())
        {
            return Err(AdmissionError::Artifacts);
        }
        artifact_bytes = artifact_bytes
            .checked_add(artifact.bytes)
            .ok_or(AdmissionError::Artifacts)?;
        previous = Some(artifact.path.as_str());
    }
    if artifacts.len() > MAX_ARTIFACTS || artifact_bytes > MAX_ARTIFACT_BYTES {
        return Err(AdmissionError::Artifacts);
    }
    Ok(())
}

fn require_hex(name: &'static str, value: &str, length: usize) -> Result<(), AdmissionError> {
    if value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(AdmissionError::Digest(name))
    }
}

/// Why a hosted request or signed result is not admissible.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    Protocol(u16),
    Provider(String),
    Digest(&'static str),
    Repository(String),
    WorkflowRef(String),
    TransportAsset,
    Architecture(String),
    Image,
    Canvas,
    Runtime(u32),
    Output(u32),
    Commands,
    Program,
    Arguments,
    Toolchain(&'static str),
    Challenge,
    Request,
    Workload,
    RunIdentity,
    Source,
    Runner,
    Isolation,
    Capture,
    Artifacts,
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(version) => write!(formatter, "unsupported hosted protocol {version}"),
            Self::Provider(provider) => {
                write!(formatter, "unsupported hosted provider {provider:?}")
            }
            Self::Digest(name) => write!(formatter, "{name} is not a lower-case SHA-256 digest"),
            Self::Repository(repository) => {
                write!(formatter, "invalid GitHub repository {repository:?}")
            }
            Self::WorkflowRef(workflow_ref) => {
                write!(formatter, "invalid hosted workflow ref {workflow_ref:?}")
            }
            Self::TransportAsset => formatter.write_str("hosted transport asset is missing"),
            Self::Architecture(architecture) => write!(
                formatter,
                "unsupported hosted architecture {architecture:?}"
            ),
            Self::Image => formatter.write_str("hosted image identity drifted or is incomplete"),
            Self::Canvas => formatter.write_str("hosted canvas names no provider run"),
            Self::Runtime(seconds) => write!(
                formatter,
                "hosted runtime bound {seconds}s is outside 1..=3600"
            ),
            Self::Output(bytes) => write!(
                formatter,
                "hosted output bound {bytes} is outside 1..=8388608"
            ),
            Self::Commands => formatter.write_str("hosted command batch count is outside 1..=8"),
            Self::Program => formatter.write_str("hosted program is empty or contains NUL"),
            Self::Arguments => {
                formatter.write_str("hosted argv exceeds its count bound or contains NUL")
            }
            Self::Toolchain(tool) => write!(
                formatter,
                "hosted toolchain pin {tool} is empty, drifted, or contains NUL"
            ),
            Self::Challenge => {
                formatter.write_str("hosted result does not answer this fresh challenge")
            }
            Self::Request => formatter.write_str("hosted result does not bind the exact request"),
            Self::Workload => {
                formatter.write_str("hosted result names a foreign repository or workflow")
            }
            Self::RunIdentity => formatter.write_str("hosted result has no provider run identity"),
            Self::Source => formatter.write_str("hosted result names changed source"),
            Self::Runner => formatter.write_str("hosted result names a changed runner executable"),
            Self::Isolation => formatter
                .write_str("hosted result did not establish the complete isolation contract"),
            Self::Capture => formatter.write_str("hosted result contains malformed output capture"),
            Self::Artifacts => {
                formatter.write_str("hosted artifact inventory is malformed or exceeds its bound")
            }
        }
    }
}

impl Error for AdmissionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Capture, Completion};
    use wire::{Digest, Domain};

    fn request() -> Request {
        Request {
            version: PROTOCOL_VERSION,
            challenge: "1".repeat(64),
            provider: GITHUB_PROVIDER.to_owned(),
            repository: "infimalabs/spice".to_owned(),
            workflow_ref: "refs/heads/main".to_owned(),
            source_commit: "0".repeat(40),
            source: "2".repeat(64),
            archive_sha256: "4".repeat(64),
            transport_asset: Some(42),
            release_inputs: None,
            tree: "3".repeat(64),
            image: ImagePin {
                label: "windows-2025".to_owned(),
                version: "20261013.288.2".to_owned(),
                os_version: "10.0.26100.33438".to_owned(),
                provider_os: "win25".to_owned(),
                architecture: X86_64_ARCHITECTURE.to_owned(),
            },
            toolchain: ToolchainPins {
                rust: "rustc 1.97.1".to_owned(),
                cargo: "cargo 1.97.1".to_owned(),
                git: "git version 2.56.0.windows.2".to_owned(),
                trunk: "trunk 0.22.1".to_owned(),
                msvc: "19.44.35222".to_owned(),
                sdk: "10.0.26100.0".to_owned(),
                rustc_path: "C:/rustc.exe".to_owned(),
                cargo_path: "C:/cargo.exe".to_owned(),
                git_path: "C:/git.exe".to_owned(),
                trunk_path: "C:/trunk.exe".to_owned(),
                cl_path: "C:/cl.exe".to_owned(),
                rc_path: "C:/rc.exe".to_owned(),
                link_path: "C:/link.exe".to_owned(),
                path: "C:/tools".to_owned(),
                include: "C:/include".to_owned(),
                lib: "C:/lib".to_owned(),
            },
            limits: Limits {
                runtime_seconds: 60,
                output_bytes: 1024,
            },
            commands: vec![Command {
                program: "@runner".to_owned(),
                arguments: vec!["space value".to_owned(), "雪".to_owned()],
            }],
            canvas: None,
        }
    }

    fn result(request: &Request) -> ResultDocument {
        ResultDocument {
            version: PROTOCOL_VERSION,
            challenge: request.challenge.clone(),
            request: request.clone(),
            identity: Identity {
                provider: request.provider.clone(),
                repository: request.repository.clone(),
                workflow: WORKFLOW.to_owned(),
                run_id: 7,
                run_attempt: 1,
                image: request.image.clone(),
                runner: Digest::compute(Domain::ProofNativeRunner, b"runner"),
                source: request.source.clone(),
                toolchain: request.toolchain.clone(),
            },
            outputs: vec![CommandResult {
                completion: Completion::Exited { code: 37 },
                stdout: Capture::from_bytes("space value\n雪\n".as_bytes(), false),
                stderr: Capture::from_bytes(b"", false),
            }],
            artifacts: Vec::new(),
            isolation: Isolation {
                source_read_only: true,
                fresh_cargo_home: true,
                fresh_target: true,
                loopback_only: true,
                process_tree_bounded: true,
            },
        }
    }

    #[test]
    fn exact_request_and_identity_admit_binary_captures() {
        let request = request();
        let result = result(&request);
        let identity =
            admit_result(&request, &result, &result.identity.runner).expect("exact result admits");
        assert_eq!(identity.run_id, 7);
        assert_eq!(
            result.outputs[0].stdout.bytes().expect("capture"),
            "space value\n雪\n".as_bytes()
        );
    }

    #[test]
    fn provider_image_revision_and_bundled_git_are_evidence_not_gates() {
        let request = request();
        let mut result = result(&request);
        result.identity.image.version = "20261115.999.1".to_owned();
        result.identity.toolchain.git = "git version 2.56.0.windows.99".to_owned();

        let identity = admit_result(&request, &result, &result.identity.runner)
            .expect("provider-only revisions admit");
        assert_eq!(identity.image.version, "20261115.999.1");
        assert_eq!(identity.toolchain.git, "git version 2.56.0.windows.99");

        let mut changed_os = result.clone();
        changed_os.identity.image.os_version = "10.0.26100.99999".to_owned();
        assert_eq!(
            admit_result(&request, &changed_os, &changed_os.identity.runner),
            Err(AdmissionError::Image)
        );

        let mut changed_compiler = result;
        changed_compiler.identity.toolchain.msvc = "19.45.99999".to_owned();
        assert_eq!(
            admit_result(
                &request,
                &changed_compiler,
                &changed_compiler.identity.runner
            ),
            Err(AdmissionError::Toolchain("observed"))
        );
    }

    #[test]
    fn output_admission_enforces_the_shared_per_command_bound() {
        let mut request = request();
        request.limits.output_bytes = 4;
        let mut exact = result(&request);
        exact.outputs[0].stdout = Capture::from_bytes(b"abc", false);
        exact.outputs[0].stderr = Capture::from_bytes(b"d", false);
        assert!(
            admit_result(&request, &exact, &exact.identity.runner).is_ok(),
            "the exact shared bound admits"
        );

        let mut over = exact;
        over.outputs[0].stderr = Capture::from_bytes(b"de", true);
        assert_eq!(
            admit_result(&request, &over, &over.identity.runner),
            Err(AdmissionError::Capture)
        );
    }

    #[test]
    fn challenge_source_runner_and_isolation_are_independent_boundaries() {
        let request = request();
        let mut replayed = result(&request);
        replayed.challenge = "3".repeat(64);
        assert_eq!(
            admit_result(&request, &replayed, &result(&request).identity.runner),
            Err(AdmissionError::Challenge)
        );
        let mut changed_request = result(&request);
        changed_request.request.archive_sha256 = "5".repeat(64);
        assert_eq!(
            admit_result(
                &request,
                &changed_request,
                &result(&request).identity.runner
            ),
            Err(AdmissionError::Request)
        );

        let mut foreign = result(&request);
        foreign.identity.repository = "other/repository".to_owned();
        assert_eq!(
            admit_result(&request, &foreign, &result(&request).identity.runner),
            Err(AdmissionError::Workload)
        );

        let mut changed = result(&request);
        changed.identity.source = "4".repeat(64);
        assert_eq!(
            admit_result(&request, &changed, &result(&request).identity.runner),
            Err(AdmissionError::Source)
        );

        let mut unsealed = result(&request);
        unsealed.isolation.loopback_only = false;
        assert_eq!(
            admit_result(&request, &unsealed, &result(&request).identity.runner),
            Err(AdmissionError::Isolation)
        );

        let changed_runner = Digest::compute(Domain::ProofNativeRunner, b"changed runner");
        assert_eq!(
            admit_result(&request, &result(&request), &changed_runner),
            Err(AdmissionError::Runner)
        );
    }
}
