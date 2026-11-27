//! Release build reporting and typed failures.

use super::*;
pub(super) fn render_records(
    output: &mut impl Write,
    build: &TargetBuild,
) -> Result<(), TargetBuildError> {
    render_command_records(output, &build.commands)?;
    writeln!(
        output,
        "[release] target={} source={} archive={} payload={} path={} executable={}",
        build.target,
        build.source,
        build.archive,
        build.staged.payload_digest,
        build.staged.payload_path.display(),
        build.executable.display()
    )
    .map_err(TargetBuildError::Output)?;
    render_canvas(output, &build.staged.canvas, build.app_tree).map_err(TargetBuildError::Output)
}

/// Name the linked canvas and what linking it moved: the executable grows by
/// the archive bytes while the payload sheds the uncompressed app tree.
pub(crate) fn render_canvas(
    output: &mut impl Write,
    canvas: &Canvas,
    app_tree: u64,
) -> io::Result<()> {
    writeln!(
        output,
        "[canvas] generation={} archive={} executable=+{} app-tree=-{} net={:+}",
        canvas.generation(),
        canvas.archive(),
        canvas.bytes(),
        app_tree,
        i128::from(canvas.bytes()) - i128::from(app_tree),
    )
}

pub(super) fn render_command_records(
    output: &mut impl Write,
    records: &[CommandRecord],
) -> Result<(), TargetBuildError> {
    for record in records {
        writeln!(
            output,
            "[release:{}] {} {:?} exit={:?}",
            record.label,
            record.program.display(),
            record.arguments,
            record.code
        )
        .map_err(TargetBuildError::Output)?;
        output
            .write_all(&record.stdout)
            .map_err(TargetBuildError::Output)?;
        output
            .write_all(&record.stderr)
            .map_err(TargetBuildError::Output)?;
    }
    Ok(())
}

/// Why the concrete target build did not publish a current pointer.
#[derive(Debug)]
pub enum TargetBuildError {
    UnsupportedTarget(String),
    TargetNotInstalled(String),
    MissingProgram(&'static str),
    Workspace(PathBuf),
    Artifact(PathBuf),
    Observation {
        label: &'static str,
        observed: String,
    },
    SourceDirty(String),
    SourceArchive(String),
    SourceMoved {
        expected: String,
        observed: String,
    },
    Launch {
        label: &'static str,
        source: io::Error,
    },
    Wait {
        label: &'static str,
        source: sill::WaitError,
    },
    Terminated(&'static str),
    IncompleteOutput {
        label: &'static str,
        stream: &'static str,
    },
    CommandFailed(CommandRecord),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    CargoConfig {
        path: PathBuf,
        detail: String,
    },
    Output(io::Error),
    Spec(sill::SpecError),
    Application(crate::app::AppBuildError),
    /// The canvas a non-Linux target adopts from the staged Linux payload.
    AdoptedCanvas(String),
    /// The cross-built bundle a prebuilt stage adopts its candidate from.
    Prebuilt(String),
    Build(dist::BuildError),
    Pins(dist::PinError),
    SourceVersion(String),
    ProofImage(String),
    Stage(dist::StageError),
    Signing(String),
}

impl fmt::Display for TargetBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTarget(target) => {
                write!(formatter, "unsupported release target {target:?}")
            }
            Self::TargetNotInstalled(target) => {
                write!(formatter, "required Rust target is not installed: {target}")
            }
            Self::MissingProgram(program) => {
                write!(formatter, "required build program is missing: {program}")
            }
            Self::Workspace(path) => write!(
                formatter,
                "not the Spice workspace root: {}",
                path.display()
            ),
            Self::Artifact(path) => write!(
                formatter,
                "build artifact is not one regular file: {}",
                path.display()
            ),
            Self::Observation { label, observed } => {
                write!(formatter, "{label} observation is invalid: {observed:?}")
            }
            Self::SourceDirty(reason) => {
                write!(formatter, "release source is not immutable: {reason}")
            }
            Self::SourceArchive(reason) => {
                write!(formatter, "release source archive refused: {reason}")
            }
            Self::SourceMoved { expected, observed } => write!(
                formatter,
                "release source moved: expected {expected}, observed {observed}"
            ),
            Self::Launch { label, source } => write!(formatter, "{label} did not launch: {source}"),
            Self::Wait { label, source } => write!(formatter, "{label} wait failed: {source}"),
            Self::Terminated(label) => write!(formatter, "{label} did not exit normally"),
            Self::IncompleteOutput { label, stream } => write!(
                formatter,
                "{label} {stream} exceeded or escaped its capture bound"
            ),
            Self::CommandFailed(record) => write!(
                formatter,
                "{} failed with exit {:?}: stdout={:?} stderr={:?}",
                record.label,
                record.code,
                String::from_utf8_lossy(&record.stdout),
                String::from_utf8_lossy(&record.stderr)
            ),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::CargoConfig { path, detail } => write!(
                formatter,
                "cargo config {} decides which rustc this build runs and cannot be read: {detail}",
                path.display()
            ),
            Self::Output(source) => write!(formatter, "release build report failed: {source}"),
            Self::Spec(source) => write!(
                formatter,
                "release process specification is invalid: {source}"
            ),
            Self::Application(error) => write!(formatter, "app build failed: {error}"),
            Self::AdoptedCanvas(reason) => {
                write!(formatter, "release canvas adoption refused: {reason}")
            }
            Self::Prebuilt(reason) => {
                write!(formatter, "prebuilt candidate refused: {reason}")
            }
            Self::Build(error) => write!(formatter, "native build failed: {error}"),
            Self::SourceVersion(detail) => write!(formatter, "product version refused: {detail}"),
            Self::ProofImage(detail) => write!(formatter, "proof image refused: {detail}"),
            Self::Pins(source) => write!(formatter, "release pin refused: {source}"),
            Self::Stage(source) => write!(formatter, "release stage failed: {source}"),
            Self::Signing(detail) => write!(formatter, "Developer ID signing refused: {detail}"),
        }
    }
}

impl Error for TargetBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Launch { source, .. } | Self::Io { source, .. } | Self::Output(source) => {
                Some(source)
            }
            Self::Wait { source, .. } => Some(source),
            Self::Spec(source) => Some(source),
            Self::Application(source) => Some(source),
            Self::Build(source) => Some(source),
            Self::Pins(source) => Some(source),
            Self::Stage(source) => Some(source),
            _ => None,
        }
    }
}

impl From<sill::SpecError> for TargetBuildError {
    fn from(source: sill::SpecError) -> Self {
        Self::Spec(source)
    }
}

impl From<dist::PinError> for TargetBuildError {
    fn from(source: dist::PinError) -> Self {
        Self::Pins(source)
    }
}

impl From<dist::StageError> for TargetBuildError {
    fn from(source: dist::StageError) -> Self {
        Self::Stage(source)
    }
}
