//! Build and restore the sealed Linux proof image without a shell pipeline.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest as _, Sha256};

const IMAGE_PLATFORM: &str = "amd64 linux";
const VENDOR_OPERATION: &str = "cargo vendor --locked --respect-source-config";
const VENDOR_SET_BUILD_ARGUMENT: &str = "spice_vendor_set";
const DEFAULT_FAILURE: u8 = 1;
const REFUSAL: u8 = 65;
const TOOL_CHECK: &str = concat!(
    "test \"$(uname -m)\" = x86_64; ",
    "test \"$(rustc --version)\" = \"rustc 1.97.1 (8bab26f4f 2026-07-14)\"; ",
    "test \"$(cargo clippy --version)\" = \"clippy 0.1.97 (8bab26f4f6 2026-07-14)\"; ",
    "test \"$(rustfmt --version)\" = \"rustfmt 1.9.0-stable (8bab26f4f6 2026-07-14)\"; ",
    "test \"$(trunk --version)\" = \"trunk 0.22.1\"; ",
    "test \"$(wasm-opt --version)\" = \"wasm-opt version 131 (version_131)\"",
);

#[derive(Clone, Copy, Default)]
struct BuildOptions<'a> {
    cache_from: Option<&'a OsStr>,
    cache_to: Option<&'a OsStr>,
    build_key: Option<&'a OsStr>,
}

impl BuildOptions<'_> {
    fn cache_configured(self) -> bool {
        self.cache_from.is_some() || self.cache_to.is_some()
    }

    fn without_cache(self) -> Self {
        Self {
            cache_from: None,
            cache_to: None,
            ..self
        }
    }
}

/// Build, admit, exercise, and export the pinned Linux proof image.
///
/// # Errors
///
/// Returns the exact failed process status, an identity refusal, or an I/O
/// failure while assembling or recording the artifact.
pub fn build_proof_image(
    root: &Path,
    archive: &Path,
    cache_from: Option<&OsStr>,
    cache_to: Option<&OsStr>,
    build_key: Option<&OsStr>,
    output: &mut impl Write,
    diagnostic: &mut impl Write,
) -> Result<(), ProofImageError> {
    let archive = resolve_archive(root, archive);
    build_with(
        &mut SystemRunner,
        root,
        &archive,
        BuildOptions {
            cache_from,
            cache_to,
            build_key,
        },
        output,
        diagnostic,
    )
}

/// Verify and restore one exported proof-image archive.
///
/// # Errors
///
/// Returns checksum or identity drift, the exact failed process status, or an
/// I/O failure while reading the artifact.
pub fn load_proof_image(
    root: &Path,
    archive: &Path,
    output: &mut impl Write,
    diagnostic: &mut impl Write,
) -> Result<(), ProofImageError> {
    let archive = resolve_archive(root, archive);
    load_with(&mut SystemRunner, root, &archive, output, diagnostic)
}

fn resolve_archive(root: &Path, archive: &Path) -> PathBuf {
    if archive.is_absolute() {
        archive.to_path_buf()
    } else {
        root.join(archive)
    }
}

fn build_with<R: Runner>(
    runner: &mut R,
    root: &Path,
    archive: &Path,
    cache: BuildOptions<'_>,
    output: &mut impl Write,
    diagnostic: &mut impl Write,
) -> Result<(), ProofImageError> {
    let state = root.join("target/proof-image");
    let context = state.join("context");
    let vendor_set = prepare_build_context(runner, root, archive, &context, output, diagnostic)?;
    let tag = vendor_set.image_tag();
    run_image_build(runner, root, &vendor_set, &tag, cache, diagnostic)?;

    let identity = admit_built_image(runner, root, &vendor_set, &tag)?;
    exercise_built_image(runner, root, &identity, output, diagnostic)?;
    export_built_image(runner, root, archive, &identity, output, diagnostic)?;
    record_archive_identity(archive, &identity)?;
    writeln!(output, "proof image {identity}").map_err(ProofImageError::Output)?;
    writeln!(output, "archive {}", archive.display()).map_err(ProofImageError::Output)?;
    Ok(())
}

fn prepare_build_context<R: Runner>(
    runner: &mut R,
    root: &Path,
    archive: &Path,
    context: &Path,
    output: &mut impl Write,
    diagnostic: &mut impl Write,
) -> Result<proof::VendorSet, ProofImageError> {
    remove_context(context)?;
    create_directory(context)?;
    create_directory(
        archive
            .parent()
            .ok_or_else(|| ProofImageError::InvalidArchive(archive.to_path_buf()))?,
    )?;

    let lock = root.join("Cargo.lock");
    let vendor_set = proof::VendorSet::read(&lock).map_err(ProofImageError::VendorSet)?;
    let vendor = context.join("vendor");
    let vendor_run = runner.run(
        VENDOR_OPERATION,
        OsStr::new("cargo"),
        &[
            OsString::from("vendor"),
            OsString::from("--locked"),
            OsString::from("--respect-source-config"),
            vendor.into_os_string(),
        ],
        root,
    )?;
    emit(&vendor_run, output, diagnostic)?;
    require_success(VENDOR_OPERATION, &vendor_run)?;

    for (source, destination) in [
        ("Dockerfile", "Dockerfile"),
        ("cargo-config.toml", "cargo-config.toml"),
        ("cargo-wrapper.sh", "cargo-wrapper.sh"),
    ] {
        copy(
            &root.join("dist/proof-image").join(source),
            &context.join(destination),
        )?;
    }
    let observed = proof::VendorSet::read(&lock).map_err(ProofImageError::VendorSet)?;
    if observed != vendor_set {
        return Err(ProofImageError::VendorSetChanged {
            before: vendor_set,
            after: observed,
        });
    }
    Ok(vendor_set)
}

fn run_image_build<R: Runner>(
    runner: &mut R,
    root: &Path,
    vendor_set: &proof::VendorSet,
    tag: &str,
    cache: BuildOptions<'_>,
    diagnostic: &mut impl Write,
) -> Result<(), ProofImageError> {
    let state = root.join("target/proof-image");
    let context = state.join("context");
    let build = run_image_build_once(runner, root, &context, vendor_set, tag, cache)?;
    let build_log = process_log(&build);
    if build.success || !cache.cache_configured() {
        record_image_build(&state, &build, &build_log)?;
        diagnostic
            .write_all(&build_log)
            .map_err(ProofImageError::Output)?;
        return require_image_build_success(&build, &build_log, &context);
    }

    const RETRY: &[u8] = b"proof-image cache unavailable; retrying one cold build\n";
    diagnostic
        .write_all(&build_log)
        .and_then(|()| diagnostic.write_all(RETRY))
        .map_err(ProofImageError::Output)?;
    let cold = run_image_build_once(
        runner,
        root,
        &context,
        vendor_set,
        tag,
        cache.without_cache(),
    )?;
    let cold_log = process_log(&cold);
    diagnostic
        .write_all(&cold_log)
        .map_err(ProofImageError::Output)?;
    let mut complete_log = Vec::with_capacity(build_log.len() + RETRY.len() + cold_log.len());
    complete_log.extend_from_slice(&build_log);
    complete_log.extend_from_slice(RETRY);
    complete_log.extend_from_slice(&cold_log);
    record_image_build(&state, &cold, &complete_log)?;
    require_image_build_success(&cold, &complete_log, &context)
}

fn run_image_build_once<R: Runner>(
    runner: &mut R,
    root: &Path,
    context: &Path,
    vendor_set: &proof::VendorSet,
    tag: &str,
    options: BuildOptions<'_>,
) -> Result<ProcessOutput, ProofImageError> {
    let mut arguments = Vec::with_capacity(
        17 + usize::from(options.cache_from.is_some()) * 2
            + usize::from(options.cache_to.is_some()) * 2
            + usize::from(options.build_key.is_some()) * 2,
    );
    arguments.extend([
        "buildx".into(),
        "build".into(),
        "--load".into(),
        "--platform".into(),
        "linux/amd64".into(),
        "--provenance=false".into(),
        "--sbom=false".into(),
        "--build-arg".into(),
        format!("{VENDOR_SET_BUILD_ARGUMENT}={vendor_set}").into(),
        "--tag".into(),
        tag.into(),
    ]);
    if let Some(cache_from) = options.cache_from {
        arguments.push("--cache-from".into());
        arguments.push(cache_from.to_owned());
    }
    if let Some(cache_to) = options.cache_to {
        arguments.push("--cache-to".into());
        arguments.push(cache_to.to_owned());
    }
    if let Some(build_key) = options.build_key {
        let mut label = OsString::from(format!("{}=", proof::PROOF_IMAGE_BUILD_KEY_LABEL));
        label.push(build_key);
        arguments.push("--label".into());
        arguments.push(label);
    }
    arguments.push(context.as_os_str().to_owned());
    runner.run(
        "docker buildx build",
        OsStr::new("docker"),
        &arguments,
        root,
    )
}

fn process_log(process: &ProcessOutput) -> Vec<u8> {
    let mut log = Vec::with_capacity(process.stdout.len() + process.stderr.len());
    log.extend_from_slice(&process.stdout);
    log.extend_from_slice(&process.stderr);
    log
}

fn record_image_build(
    state: &Path,
    build: &ProcessOutput,
    build_log: &[u8],
) -> Result<(), ProofImageError> {
    write(&state.join("build.log"), build_log)?;
    write(
        &state.join("build.status"),
        format!(
            "{}\n",
            build
                .code
                .map_or_else(|| "signal".to_owned(), |code| code.to_string())
        )
        .as_bytes(),
    )
}

fn require_image_build_success(
    build: &ProcessOutput,
    build_log: &[u8],
    context: &Path,
) -> Result<(), ProofImageError> {
    if build.success {
        return Ok(());
    }
    if contains(build_log, b"unexpected EOF") {
        return Err(ProofImageError::DroppedBuild {
            code: build.code,
            context: context.to_path_buf(),
        });
    }
    Err(ProofImageError::Exit {
        operation: "docker buildx build",
        code: build.code,
    })
}

fn admit_built_image<R: Runner>(
    runner: &mut R,
    root: &Path,
    vendor_set: &proof::VendorSet,
    tag: &str,
) -> Result<String, ProofImageError> {
    let inspect = runner.run(
        "docker image inspect",
        OsStr::new("docker"),
        &[
            "image".into(),
            "inspect".into(),
            "--format".into(),
            format!(
                "{{{{.Id}}}} {{{{.Architecture}}}} {{{{.Os}}}} {{{{index .Config.Labels \"{}\"}}}}",
                proof::VENDOR_SET_LABEL
            )
            .into(),
            tag.into(),
        ],
        root,
    )?;
    require_success("docker image inspect", &inspect)?;
    let observed = one_line("docker image inspect", &inspect.stdout)?;
    admit_inspected_image("built proof image", &observed, None, vendor_set)
}

fn admit_inspected_image(
    subject: &'static str,
    observed: &str,
    expected_identity: Option<&str>,
    vendor_set: &proof::VendorSet,
) -> Result<String, ProofImageError> {
    let Some((observed_image, observed_vendor_set)) = observed.rsplit_once(' ') else {
        return Err(ProofImageError::MalformedOutput {
            operation: "docker image inspect",
            observed: observed.to_owned(),
        });
    };
    let Some((identity, platform)) = observed_image.split_once(' ') else {
        return Err(ProofImageError::MalformedOutput {
            operation: "docker image inspect",
            observed: observed.to_owned(),
        });
    };
    identity
        .parse::<wire::manifest::ProofImage>()
        .map_err(|_| ProofImageError::IdentityDrift {
            subject,
            expected: "one exact SHA-256 image ID".to_owned(),
            observed: identity.to_owned(),
        })?;
    if let Some(expected) = expected_identity
        && identity != expected
    {
        return Err(ProofImageError::IdentityDrift {
            subject,
            expected: expected.to_owned(),
            observed: identity.to_owned(),
        });
    }
    if platform != IMAGE_PLATFORM {
        return Err(ProofImageError::IdentityDrift {
            subject,
            expected: format!("{identity} {IMAGE_PLATFORM}"),
            observed: observed_image.to_owned(),
        });
    }
    if observed_vendor_set != vendor_set.as_str() {
        return Err(ProofImageError::VendorSetDrift {
            expected: vendor_set.to_string(),
            observed: observed_vendor_set.to_owned(),
        });
    }
    Ok(identity.to_owned())
}

fn exercise_built_image<R: Runner>(
    runner: &mut R,
    root: &Path,
    identity: &str,
    output: &mut impl Write,
    diagnostic: &mut impl Write,
) -> Result<(), ProofImageError> {
    let check = runner.run(
        "docker run proof-image self-check",
        OsStr::new("docker"),
        &[
            "run".into(),
            "--rm".into(),
            "--network=none".into(),
            "--platform".into(),
            "linux/amd64".into(),
            identity.into(),
            "sh".into(),
            "-ceu".into(),
            TOOL_CHECK.into(),
        ],
        root,
    )?;
    emit(&check, output, diagnostic)?;
    require_success("docker run proof-image self-check", &check)?;
    Ok(())
}

fn export_built_image<R: Runner>(
    runner: &mut R,
    root: &Path,
    archive: &Path,
    identity: &str,
    output: &mut impl Write,
    diagnostic: &mut impl Write,
) -> Result<(), ProofImageError> {
    let save = runner.run(
        "docker save proof image",
        OsStr::new("docker"),
        &[
            "save".into(),
            "--output".into(),
            archive.as_os_str().to_owned(),
            identity.into(),
        ],
        root,
    )?;
    emit(&save, output, diagnostic)?;
    require_success("docker save proof image", &save)?;
    Ok(())
}

fn record_archive_identity(archive: &Path, identity: &str) -> Result<(), ProofImageError> {
    let checksum = sha256(archive)?;
    write(
        &sidecar(archive, "sha256"),
        format!("{checksum}\n").as_bytes(),
    )?;
    write(
        &sidecar(archive, "identity"),
        format!("{identity}\n").as_bytes(),
    )?;
    Ok(())
}

fn load_with<R: Runner>(
    runner: &mut R,
    root: &Path,
    archive: &Path,
    output: &mut impl Write,
    diagnostic: &mut impl Write,
) -> Result<(), ProofImageError> {
    let vendor_set =
        proof::VendorSet::read(root.join("Cargo.lock")).map_err(ProofImageError::VendorSet)?;
    let expected_checksum = read_one_line(&sidecar(archive, "sha256"))?;
    let observed_checksum = sha256(archive)?;
    if observed_checksum != expected_checksum {
        return Err(ProofImageError::ChecksumDrift {
            expected: expected_checksum,
            observed: observed_checksum,
        });
    }
    let archived = read_one_line(&sidecar(archive, "identity"))?;
    archived
        .parse::<wire::manifest::ProofImage>()
        .map_err(|_| ProofImageError::IdentityDrift {
            subject: "proof archive",
            expected: "one exact SHA-256 image ID".to_owned(),
            observed: archived.clone(),
        })?;

    let load = runner.run(
        "docker load proof image",
        OsStr::new("docker"),
        &[
            "load".into(),
            "--input".into(),
            archive.as_os_str().to_owned(),
        ],
        root,
    )?;
    emit(&load, output, diagnostic)?;
    require_success("docker load proof image", &load)?;

    let inspect = runner.run(
        "docker image inspect",
        OsStr::new("docker"),
        &[
            "image".into(),
            "inspect".into(),
            "--format".into(),
            format!(
                "{{{{.Id}}}} {{{{.Architecture}}}} {{{{.Os}}}} {{{{index .Config.Labels \"{}\"}}}}",
                proof::VENDOR_SET_LABEL
            )
            .into(),
            archived.clone().into(),
        ],
        root,
    )?;
    require_success("docker image inspect", &inspect)?;
    let observed = one_line("docker image inspect", &inspect.stdout)?;
    let loaded = admit_inspected_image(
        "loaded proof image",
        &observed,
        Some(&archived),
        &vendor_set,
    )?;
    let tag = vendor_set.image_tag();
    let tagged = runner.run(
        "docker image tag",
        OsStr::new("docker"),
        &[
            "image".into(),
            "tag".into(),
            loaded.clone().into(),
            tag.into(),
        ],
        root,
    )?;
    require_success("docker image tag", &tagged)?;
    writeln!(output, "loaded proof image {loaded}").map_err(ProofImageError::Output)?;
    Ok(())
}

fn remove_context(path: &Path) -> Result<(), ProofImageError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ProofImageError::Io {
            operation: "remove prior proof-image build context",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn create_directory(path: &Path) -> Result<(), ProofImageError> {
    fs::create_dir_all(path).map_err(|source| ProofImageError::Io {
        operation: "create proof-image directory",
        path: path.to_path_buf(),
        source,
    })
}

fn copy(source: &Path, destination: &Path) -> Result<(), ProofImageError> {
    fs::copy(source, destination)
        .map(|_| ())
        .map_err(|error| ProofImageError::Io {
            operation: "copy proof-image input",
            path: source.to_path_buf(),
            source: error,
        })
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), ProofImageError> {
    fs::write(path, bytes).map_err(|source| ProofImageError::Io {
        operation: "write proof-image artifact",
        path: path.to_path_buf(),
        source,
    })
}

fn read_one_line(path: &Path) -> Result<String, ProofImageError> {
    let bytes = fs::read(path).map_err(|source| ProofImageError::Io {
        operation: "read proof-image artifact",
        path: path.to_path_buf(),
        source,
    })?;
    one_line("proof-image artifact", &bytes)
}

fn one_line(operation: &'static str, bytes: &[u8]) -> Result<String, ProofImageError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ProofImageError::MalformedOutput {
        operation,
        observed: String::from_utf8_lossy(bytes).into_owned(),
    })?;
    let line = text.trim_ascii_end();
    if line.is_empty() || line.contains(['\n', '\r']) {
        return Err(ProofImageError::MalformedOutput {
            operation,
            observed: text.to_owned(),
        });
    }
    Ok(line.to_owned())
}

fn sha256(path: &Path) -> Result<String, ProofImageError> {
    let mut file = File::open(path).map_err(|source| ProofImageError::Io {
        operation: "open proof-image archive for checksum",
        path: path.to_path_buf(),
        source,
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| ProofImageError::Io {
                operation: "read proof-image archive for checksum",
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn sidecar(archive: &Path, suffix: &str) -> PathBuf {
    let mut path = archive.as_os_str().to_owned();
    path.push(format!(".{suffix}"));
    PathBuf::from(path)
}

fn emit(
    process: &ProcessOutput,
    output: &mut impl Write,
    diagnostic: &mut impl Write,
) -> Result<(), ProofImageError> {
    output
        .write_all(&process.stdout)
        .map_err(ProofImageError::Output)?;
    diagnostic
        .write_all(&process.stderr)
        .map_err(ProofImageError::Output)
}

fn require_success(
    operation: &'static str,
    process: &ProcessOutput,
) -> Result<(), ProofImageError> {
    if process.success {
        Ok(())
    } else {
        Err(ProofImageError::Exit {
            operation,
            code: process.code,
        })
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|candidate| candidate == needle)
}

#[derive(Debug)]
struct ProcessOutput {
    success: bool,
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

trait Runner {
    fn run(
        &mut self,
        operation: &'static str,
        program: &OsStr,
        arguments: &[OsString],
        root: &Path,
    ) -> Result<ProcessOutput, ProofImageError>;
}

struct SystemRunner;

impl Runner for SystemRunner {
    fn run(
        &mut self,
        operation: &'static str,
        program: &OsStr,
        arguments: &[OsString],
        root: &Path,
    ) -> Result<ProcessOutput, ProofImageError> {
        let output = Command::new(program)
            .args(arguments)
            .current_dir(root)
            .output()
            .map_err(|source| ProofImageError::Launch { operation, source })?;
        Ok(ProcessOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// A proof-image build or restore refusal.
#[derive(Debug)]
pub enum ProofImageError {
    Output(io::Error),
    InvalidArchive(PathBuf),
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Launch {
        operation: &'static str,
        source: io::Error,
    },
    VendorSet(proof::VendorSetError),
    VendorSetChanged {
        before: proof::VendorSet,
        after: proof::VendorSet,
    },
    Exit {
        operation: &'static str,
        code: Option<i32>,
    },
    DroppedBuild {
        code: Option<i32>,
        context: PathBuf,
    },
    MalformedOutput {
        operation: &'static str,
        observed: String,
    },
    IdentityDrift {
        subject: &'static str,
        expected: String,
        observed: String,
    },
    ChecksumDrift {
        expected: String,
        observed: String,
    },
    VendorSetDrift {
        expected: String,
        observed: String,
    },
}

impl ProofImageError {
    /// Process exit status preserved for the xtask process boundary.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Exit { code, .. } | Self::DroppedBuild { code, .. } => code
                .and_then(|code| u8::try_from(code).ok())
                .filter(|code| *code != 0)
                .unwrap_or(DEFAULT_FAILURE),
            Self::IdentityDrift { .. }
            | Self::ChecksumDrift { .. }
            | Self::VendorSetChanged { .. }
            | Self::VendorSetDrift { .. }
            | Self::MalformedOutput { .. } => REFUSAL,
            Self::Output(_)
            | Self::InvalidArchive(_)
            | Self::Io { .. }
            | Self::Launch { .. }
            | Self::VendorSet(_) => DEFAULT_FAILURE,
        }
    }
}

impl fmt::Display for ProofImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output(source) => write!(formatter, "proof-image output failed: {source}"),
            Self::InvalidArchive(path) => {
                write!(
                    formatter,
                    "proof-image archive has no parent: {}",
                    path.display()
                )
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
            Self::Launch { operation, source } => {
                write!(formatter, "failed to launch {operation}: {source}")
            }
            Self::VendorSet(source) => {
                write!(formatter, "cannot derive proof-image vendor set: {source}")
            }
            Self::VendorSetChanged { before, after } => write!(
                formatter,
                "Cargo.lock vendor set changed while preparing the proof image: before {before}, \
                 after {after}"
            ),
            Self::Exit {
                operation,
                code: Some(code),
            } => write!(formatter, "{operation} failed with exit {code}"),
            Self::Exit {
                operation,
                code: None,
            } => write!(formatter, "{operation} terminated by signal"),
            Self::DroppedBuild {
                code: Some(code),
                context,
            } => write!(
                formatter,
                "docker buildx build failed with exit {code}: the container runtime dropped this \
                 build; no compiler refused it. On a shared daemon an unexpected EOF usually \
                 means a concurrent global prune took the build cache and pinned proof image \
                 from every lane. Clean up only this lane's disposable build context, {}, and \
                 rerun",
                context.display(),
            ),
            Self::DroppedBuild {
                code: None,
                context,
            } => write!(
                formatter,
                "docker buildx build terminated by signal after an unexpected EOF: the container \
                 runtime dropped this build; no compiler refused it. A concurrent global prune \
                 usually took the shared cache and pinned image. Clean up only this lane's \
                 disposable build context, {}, and rerun",
                context.display(),
            ),
            Self::MalformedOutput {
                operation,
                observed,
            } => write!(
                formatter,
                "{operation} emitted no single identity line: {observed:?}"
            ),
            Self::IdentityDrift {
                subject,
                expected,
                observed,
            } => write!(
                formatter,
                "{subject} identity drift: expected {expected}, observed {observed}",
            ),
            Self::ChecksumDrift { expected, observed } => write!(
                formatter,
                "proof archive checksum drift: expected {expected}, observed {observed}",
            ),
            Self::VendorSetDrift { expected, observed } => write!(
                formatter,
                "built proof image vendor-set label drift: expected {expected}, observed {observed}",
            ),
        }
    }
}

impl std::error::Error for ProofImageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Output(source) | Self::Io { source, .. } | Self::Launch { source, .. } => {
                Some(source)
            }
            Self::VendorSet(source) => Some(source),
            Self::InvalidArchive(_)
            | Self::Exit { .. }
            | Self::DroppedBuild { .. }
            | Self::MalformedOutput { .. }
            | Self::IdentityDrift { .. }
            | Self::ChecksumDrift { .. }
            | Self::VendorSetChanged { .. }
            | Self::VendorSetDrift { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const IDENTITY: &str =
        "sha256:2d5976ad79b7a4e929a115d8d10ab7b8bd00bb633b5ad6d459d048eb2f817f8a";
    const BUILD_KEY: &str =
        "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    #[test]
    fn build_records_the_streamed_archive_checksum_and_admitted_identity() {
        let fixture = Fixture::new();
        let archive = fixture.archive();
        let vendor_set = fixture.vendor_set();
        fs::write(&archive, b"abc").expect("write fixture archive");
        let mut runner = FakeRunner::new([
            success(b"vendor\n", b""),
            success(b"build\n", b"progress\n"),
            success(
                format!("{IDENTITY} {IMAGE_PLATFORM} {vendor_set}\n").as_bytes(),
                b"",
            ),
            success(b"checked\n", b""),
            success(b"saved\n", b""),
        ]);
        let mut output = Vec::new();
        let mut diagnostic = Vec::new();

        build_with(
            &mut runner,
            &fixture.root,
            &archive,
            BuildOptions::default(),
            &mut output,
            &mut diagnostic,
        )
        .expect("proof image builds");

        assert_eq!(
            fs::read_to_string(sidecar(&archive, "sha256")).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n"
        );
        assert_eq!(
            fs::read_to_string(sidecar(&archive, "identity")).unwrap(),
            format!("{IDENTITY}\n")
        );
        assert_eq!(
            fs::read(fixture.root.join("target/proof-image/build.log")).unwrap(),
            b"build\nprogress\n"
        );
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("proof image sha256:")
        );
        assert_eq!(runner.calls.len(), 5);
        assert_eq!(runner.calls[0].operation, VENDOR_OPERATION);
        assert_eq!(runner.calls[0].program, OsString::from("cargo"));
        assert_eq!(
            runner.calls[0].arguments,
            [
                OsString::from("vendor"),
                OsString::from("--locked"),
                OsString::from("--respect-source-config"),
                fixture
                    .root
                    .join("target/proof-image/context/vendor")
                    .into_os_string(),
            ]
        );
        assert!(
            runner.calls[1].arguments.windows(2).any(|arguments| {
                arguments
                    == [
                        OsString::from("--build-arg"),
                        OsString::from(format!("{VENDOR_SET_BUILD_ARGUMENT}={vendor_set}")),
                    ]
            }),
            "image build did not receive the admitted vendor set"
        );
        assert!(runner.calls[1].arguments.windows(2).any(|arguments| {
            arguments
                == [
                    OsString::from("--tag"),
                    OsString::from(
                        proof::VendorSet::read(fixture.root.join("Cargo.lock"))
                            .unwrap()
                            .image_tag(),
                    ),
                ]
        }));
    }

    #[test]
    fn cache_options_reach_buildx_without_changing_identity_admission() {
        let fixture = Fixture::new();
        let archive = fixture.archive();
        let vendor_set = fixture.vendor_set();
        fs::write(&archive, b"abc").expect("write fixture archive");
        let mut runner = FakeRunner::new([
            success(b"", b""),
            success(b"cached build\n", b""),
            success(
                format!("{IDENTITY} {IMAGE_PLATFORM} {vendor_set}\n").as_bytes(),
                b"",
            ),
            success(b"", b""),
            success(b"", b""),
        ]);

        build_with(
            &mut runner,
            &fixture.root,
            &archive,
            BuildOptions {
                cache_from: Some(OsStr::new("type=gha,scope=proof-image")),
                cache_to: Some(OsStr::new(
                    "type=gha,mode=max,scope=proof-image,ignore-error=true",
                )),
                build_key: Some(OsStr::new(BUILD_KEY)),
            },
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect("cached proof image builds");

        let build = &runner.calls[1].arguments;
        for expected in [
            ["--cache-from", "type=gha,scope=proof-image"],
            [
                "--cache-to",
                "type=gha,mode=max,scope=proof-image,ignore-error=true",
            ],
            [
                "--label",
                "org.spice.build-key=sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            ],
        ] {
            assert!(
                build
                    .windows(2)
                    .any(|arguments| arguments == expected.map(OsString::from)),
                "image build omitted {expected:?}"
            );
        }
        assert_eq!(
            fs::read_to_string(sidecar(&archive, "identity")).unwrap(),
            format!("{IDENTITY}\n")
        );
    }

    #[test]
    fn unavailable_cache_retries_one_cold_build() {
        let fixture = Fixture::new();
        let archive = fixture.archive();
        let vendor_set = fixture.vendor_set();
        fs::write(&archive, b"abc").expect("write fixture archive");
        let mut runner = FakeRunner::new([
            success(b"", b""),
            failed(1, b"", b"cache service unavailable\n"),
            success(b"cold build\n", b""),
            success(
                format!("{IDENTITY} {IMAGE_PLATFORM} {vendor_set}\n").as_bytes(),
                b"",
            ),
            success(b"", b""),
            success(b"", b""),
        ]);

        build_with(
            &mut runner,
            &fixture.root,
            &archive,
            BuildOptions {
                cache_from: Some(OsStr::new("type=gha,scope=proof-image")),
                cache_to: Some(OsStr::new(
                    "type=gha,mode=max,scope=proof-image,ignore-error=true",
                )),
                build_key: Some(OsStr::new(BUILD_KEY)),
            },
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect("cache outage degrades to a cold build");

        assert!(
            runner.calls[1]
                .arguments
                .contains(&OsString::from("--cache-from"))
        );
        assert!(
            !runner.calls[2]
                .arguments
                .contains(&OsString::from("--cache-from"))
        );
        assert!(
            !runner.calls[2]
                .arguments
                .contains(&OsString::from("--cache-to"))
        );
        assert!(runner.calls[2].arguments.windows(2).any(|arguments| {
            arguments
                == [
                    OsString::from("--label"),
                    OsString::from(format!(
                        "{}={BUILD_KEY}",
                        proof::PROOF_IMAGE_BUILD_KEY_LABEL
                    )),
                ]
        }));
        let log = fs::read_to_string(fixture.root.join("target/proof-image/build.log")).unwrap();
        assert!(log.contains("cache service unavailable"));
        assert!(log.contains("retrying one cold build"));
        assert!(log.contains("cold build"));
    }

    #[test]
    fn build_inspection_requires_an_exact_image_id() {
        let fixture = Fixture::new();
        let vendor_set = fixture.vendor_set();
        let mut runner = FakeRunner::new([
            success(b"", b""),
            success(b"", b""),
            success(
                format!("not-an-image {IMAGE_PLATFORM} {vendor_set}\n").as_bytes(),
                b"",
            ),
        ]);

        let error = build_with(
            &mut runner,
            &fixture.root,
            &fixture.archive(),
            BuildOptions::default(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect_err("a malformed image ID refuses");

        assert!(matches!(error, ProofImageError::IdentityDrift { .. }));
        assert_eq!(error.exit_code(), REFUSAL);
    }
    #[test]
    fn build_vendor_set_label_must_equal_the_lockfile_set() {
        let fixture = Fixture::new();
        let mut runner = FakeRunner::new([
            success(b"", b""),
            success(b"", b""),
            success(
                format!("{IDENTITY} {IMAGE_PLATFORM} sha256:{}\n", "9".repeat(64)).as_bytes(),
                b"",
            ),
        ]);

        let error = build_with(
            &mut runner,
            &fixture.root,
            &fixture.archive(),
            BuildOptions::default(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect_err("vendor-set label drift refuses");

        assert!(matches!(error, ProofImageError::VendorSetDrift { .. }));
        assert_eq!(error.exit_code(), REFUSAL);
    }

    #[test]
    fn unexpected_eof_names_the_shared_prune_and_preserves_the_build_exit() {
        let fixture = Fixture::new();
        let mut runner = FakeRunner::new([
            success(b"", b""),
            failed(73, b"", b"failed to solve: unexpected EOF\n"),
        ]);

        let error = build_with(
            &mut runner,
            &fixture.root,
            &fixture.archive(),
            BuildOptions::default(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect_err("a dropped build refuses");

        assert_eq!(error.exit_code(), 73);
        let diagnostic = error.to_string();
        for text in [
            "no compiler refused",
            "shared daemon",
            "global prune",
            "every lane",
        ] {
            assert!(diagnostic.contains(text), "missing {text:?}: {diagnostic}");
        }
        assert_disposable_context(&diagnostic, &fixture.root);
        assert_eq!(
            fs::read_to_string(fixture.root.join("target/proof-image/build.status")).unwrap(),
            "73\n"
        );
    }

    #[test]
    fn unexpected_eof_after_a_signal_names_only_the_disposable_context() {
        let fixture = Fixture::new();
        let mut runner = FakeRunner::new([
            success(b"", b""),
            terminated(b"", b"failed to solve: unexpected EOF\n"),
        ]);

        let error = build_with(
            &mut runner,
            &fixture.root,
            &fixture.archive(),
            BuildOptions::default(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect_err("a signalled build refuses");

        assert_eq!(error.exit_code(), DEFAULT_FAILURE);
        assert_disposable_context(&error.to_string(), &fixture.root);
        assert_eq!(
            fs::read_to_string(fixture.root.join("target/proof-image/build.status")).unwrap(),
            "signal\n"
        );
    }

    #[test]
    fn loader_refuses_archive_checksum_drift_before_contacting_docker() {
        let fixture = Fixture::new();
        let archive = fixture.archive();
        fs::write(&archive, b"archive").unwrap();
        fs::write(sidecar(&archive, "sha256"), "not-the-checksum\n").unwrap();
        fs::write(sidecar(&archive, "identity"), format!("{IDENTITY}\n")).unwrap();
        let mut runner = FakeRunner::new([]);

        let error = load_with(
            &mut runner,
            &fixture.root,
            &archive,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect_err("checksum drift refuses");

        assert!(matches!(error, ProofImageError::ChecksumDrift { .. }));
        assert!(runner.calls.is_empty());
    }

    #[test]
    fn loader_admits_the_archive_identity_and_rechecks_the_loaded_image() {
        let fixture = Fixture::new();
        let archive = fixture.archive();
        fs::write(&archive, b"abc").unwrap();
        fs::write(
            sidecar(&archive, "sha256"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n",
        )
        .unwrap();
        fs::write(sidecar(&archive, "identity"), format!("{IDENTITY}\n")).unwrap();
        let vendor_set = fixture.vendor_set();
        let mut runner = FakeRunner::new([
            success(b"loaded archive\n", b""),
            success(
                format!("{IDENTITY} {IMAGE_PLATFORM} {vendor_set}\n").as_bytes(),
                b"",
            ),
            success(b"", b""),
        ]);
        let mut output = Vec::new();

        load_with(
            &mut runner,
            &fixture.root,
            &archive,
            &mut output,
            &mut Vec::new(),
        )
        .expect("archive loads");

        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("loaded proof image")
        );
        assert_eq!(runner.calls.len(), 3);
    }

    #[test]
    fn loader_refuses_an_archive_for_another_vendor_set() {
        let fixture = Fixture::new();
        let archive = fixture.archive();
        fs::write(&archive, b"abc").unwrap();
        fs::write(
            sidecar(&archive, "sha256"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n",
        )
        .unwrap();
        fs::write(sidecar(&archive, "identity"), format!("{IDENTITY}\n")).unwrap();
        let mut runner = FakeRunner::new([
            success(b"loaded archive\n", b""),
            success(
                format!("{IDENTITY} {IMAGE_PLATFORM} sha256:{}\n", "9".repeat(64)).as_bytes(),
                b"",
            ),
        ]);

        let error = load_with(
            &mut runner,
            &fixture.root,
            &archive,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect_err("an archive for another vendor set refuses");

        assert!(matches!(error, ProofImageError::VendorSetDrift { .. }));
        assert_eq!(runner.calls.len(), 2, "a refused image is never tagged");
    }

    struct FakeRunner {
        outcomes: VecDeque<ProcessOutput>,
        calls: Vec<ProcessCall>,
    }

    struct ProcessCall {
        operation: &'static str,
        program: OsString,
        arguments: Vec<OsString>,
    }

    impl FakeRunner {
        fn new(outcomes: impl IntoIterator<Item = ProcessOutput>) -> Self {
            Self {
                outcomes: outcomes.into_iter().collect(),
                calls: Vec::new(),
            }
        }
    }

    impl Runner for FakeRunner {
        fn run(
            &mut self,
            operation: &'static str,
            program: &OsStr,
            arguments: &[OsString],
            _root: &Path,
        ) -> Result<ProcessOutput, ProofImageError> {
            self.calls.push(ProcessCall {
                operation,
                program: program.to_owned(),
                arguments: arguments.to_vec(),
            });
            Ok(self
                .outcomes
                .pop_front()
                .expect("fixture has one process outcome"))
        }
    }

    fn success(stdout: &[u8], stderr: &[u8]) -> ProcessOutput {
        ProcessOutput {
            success: true,
            code: Some(0),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    fn failed(code: i32, stdout: &[u8], stderr: &[u8]) -> ProcessOutput {
        ProcessOutput {
            success: false,
            code: Some(code),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    fn terminated(stdout: &[u8], stderr: &[u8]) -> ProcessOutput {
        ProcessOutput {
            success: false,
            code: None,
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    fn assert_disposable_context(diagnostic: &str, root: &Path) {
        let context = root.join("target/proof-image/context");
        assert!(
            diagnostic.ends_with(&format!(
                "Clean up only this lane's disposable build context, {}, and rerun",
                context.display()
            )),
            "unsafe cleanup diagnostic: {diagnostic}"
        );
    }

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "spice-proof-image-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let source = root.join("dist/proof-image");
            fs::create_dir_all(&source).expect("create fixture proof-image inputs");
            for name in ["Dockerfile", "cargo-config.toml", "cargo-wrapper.sh"] {
                fs::write(source.join(name), format!("fixture {name}\n")).unwrap();
            }
            fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
            fs::create_dir_all(root.join("target/proof-image"))
                .expect("create fixture proof-image state");
            Self { root }
        }

        fn archive(&self) -> PathBuf {
            self.root.join("target/proof-image/image.tar")
        }
        fn vendor_set(&self) -> String {
            proof::VendorSet::read(self.root.join("Cargo.lock"))
                .expect("fixture vendor set")
                .to_string()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove proof-image fixture");
        }
    }
}
