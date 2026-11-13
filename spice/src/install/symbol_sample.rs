//! Bounded symbolization of one `sample(1)` report through the retained dSYM.
//!
//! The operator captures a short sample of the installed executable with the
//! system sampler and hands the text file here. Only addresses the report
//! attributes to the installed image are resolved, each through one bounded
//! `dwarfdump --lookup` launch against the verified DWARF, so a report can
//! never cost more than a fixed number of fixed-length lookups.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use sill::{
    Argv, Cancellation, CancellationToken, Completion, Deadlines, Environment, Output,
    ProcessGroup, Redaction, RestartDisposition, Spec, StdinPolicy, StreamPolicy, WaitPolicy,
};

use wire::manifest::macho_uuid_is_canonical;

use super::{Repository, SymbolVerification};

/// Largest sample report the symbolizer reads.
pub const MAX_SAMPLE_BYTES: u64 = 4 * 1024 * 1024;
/// Most distinct installed-image addresses one report may carry.
pub const MAX_SAMPLE_ADDRESSES: usize = 256;
const LOOKUP_RUNTIME: Duration = Duration::from_secs(5);
const LOOKUP_OUTPUT_DRAIN: Duration = Duration::from_millis(250);
const LOOKUP_REAP: Duration = Duration::from_millis(500);
const LOOKUP_GRACE: Duration = Duration::from_millis(100);
const MAX_LOOKUP_BYTES: usize = 64 * 1024;
const DWARFDUMP: &str = "dwarfdump";

/// A bounded tool resolving one DWARF address per launch.
pub trait DwarfLookup {
    /// The textual `dwarfdump --lookup ADDRESS --show-parents DWARF` report.
    ///
    /// # Errors
    ///
    /// The tool failed to launch, exceeded its bounds, or exited unsuccessfully.
    fn lookup(&self, dwarf: &Path, address: u64) -> Result<String, LookupError>;
}

/// A lookup tool failure with its complete diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LookupError(String);

impl fmt::Display for LookupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for LookupError {}

/// The system `dwarfdump` launched once per address under fixed bounds.
pub struct DwarfdumpLookup {
    cwd: PathBuf,
    environment: Environment,
}

impl DwarfdumpLookup {
    /// A lookup tool resolved through the supplied rim environment.
    #[must_use]
    pub const fn new(cwd: PathBuf, environment: Environment) -> Self {
        Self { cwd, environment }
    }

    fn spec(&self, dwarf: &Path, address: u64) -> Result<Spec, LookupError> {
        let capture = StreamPolicy::Capture {
            limit: MAX_LOOKUP_BYTES,
        };
        let arguments = [
            "--lookup".to_owned(),
            format!("{address:#x}"),
            "--show-parents".to_owned(),
            dwarf.display().to_string(),
        ];
        Ok(Spec {
            argv: Argv::new(DWARFDUMP, arguments).map_err(spec_error)?,
            cwd: self.cwd.clone(),
            environment: self.environment.clone(),
            group: ProcessGroup::New,
            deadlines: Deadlines::new(LOOKUP_RUNTIME, LOOKUP_OUTPUT_DRAIN, LOOKUP_REAP)
                .map_err(spec_error)?,
            wait: WaitPolicy::new(
                "install symbols lookup",
                Cancellation::Token(CancellationToken::new()),
                RestartDisposition::Never,
            )
            .map_err(spec_error)?,
            output: Output::new(StdinPolicy::Null, capture, capture),
            redaction: Redaction::None,
            grace: LOOKUP_GRACE,
        })
    }
}

fn spec_error(error: sill::SpecError) -> LookupError {
    LookupError(format!("{DWARFDUMP} process contract is invalid: {error}"))
}

impl DwarfLookup for DwarfdumpLookup {
    fn lookup(&self, dwarf: &Path, address: u64) -> Result<String, LookupError> {
        let child = sill::spawn(self.spec(dwarf, address)?)
            .map_err(|error| LookupError(format!("could not launch {DWARFDUMP}: {error}")))?;
        let outcome = child
            .wait()
            .map_err(|error| LookupError(format!("{DWARFDUMP} boundary failed: {error}")))?;
        if !outcome.outputs_drained() {
            return Err(LookupError(format!(
                "{DWARFDUMP} left output undrained at its boundary"
            )));
        }
        match outcome.completion() {
            Completion::TimedOut(_) => {
                return Err(LookupError(format!(
                    "{DWARFDUMP} exceeded {}ms",
                    LOOKUP_RUNTIME.as_millis()
                )));
            }
            Completion::Cancelled(_) => {
                return Err(LookupError(format!("{DWARFDUMP} was cancelled")));
            }
            Completion::Exited(_) => {}
        }
        let stdout = captured(outcome.stdout(), "stdout")?;
        let stderr = captured(outcome.stderr(), "stderr")?;
        let status = outcome.completion().status();
        // An address no compile unit covers exits 1 with the bare file-format
        // header and nothing on stderr; that is an unresolved frame, not a
        // tool failure.
        if status.success() || (status.code() == Some(1) && stderr.is_empty()) {
            return Ok(stdout);
        }
        Err(LookupError(format!(
            "{DWARFDUMP} exited with {}: {}",
            status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stderr.trim(),
        )))
    }
}

fn captured(output: Option<&sill::CapturedOutput>, stream: &str) -> Result<String, LookupError> {
    let output = output
        .ok_or_else(|| LookupError(format!("{DWARFDUMP} {stream} capture was unavailable")))?;
    if output.truncated() {
        return Err(LookupError(format!(
            "{DWARFDUMP} {stream} exceeded {MAX_LOOKUP_BYTES} bytes"
        )));
    }
    if !output.complete() {
        return Err(LookupError(format!(
            "{DWARFDUMP} {stream} did not close within the output deadline"
        )));
    }
    String::from_utf8(output.bytes().to_vec())
        .map_err(|error| LookupError(format!("{DWARFDUMP} {stream} was not UTF-8: {error}")))
}

/// One source position named by DWARF.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceLocation {
    file: String,
    line: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    column: Option<u64>,
}

impl SourceLocation {
    /// Source file as DWARF spells it.
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    /// One-based source line.
    #[must_use]
    pub const fn line(&self) -> u64 {
        self.line
    }
}

/// The named function and source positions one address resolved to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SampledFrame {
    function: String,
    declared: SourceLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<SourceLocation>,
}

impl SampledFrame {
    /// Namespace-qualified Rust function name, as DWARF declares it.
    #[must_use]
    pub fn function(&self) -> &str {
        &self.function
    }

    /// Where the function is declared.
    #[must_use]
    pub const fn declared(&self) -> &SourceLocation {
        &self.declared
    }

    /// The exact line the address maps to, when the line table names one.
    #[must_use]
    pub const fn line(&self) -> Option<&SourceLocation> {
        self.line.as_ref()
    }
}

/// One distinct sampled address and what the retained DWARF says about it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SymbolizedFrame {
    address: String,
    file_address: String,
    samples: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    frame: Option<SampledFrame>,
}

impl SymbolizedFrame {
    /// Runtime address as the sample spelled it.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Deepest sample count the report attributed to this address.
    #[must_use]
    pub const fn samples(&self) -> u64 {
        self.samples
    }

    /// The resolved frame, absent when DWARF names nothing at the address.
    #[must_use]
    pub const fn frame(&self) -> Option<&SampledFrame> {
        self.frame.as_ref()
    }
}

/// One sample report resolved through the verified installed dSYM.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SampleSymbolization {
    sample: String,
    load_address: String,
    uuid: String,
    addresses: usize,
    resolved: usize,
    frames: Vec<SymbolizedFrame>,
}

impl SampleSymbolization {
    /// Distinct installed-image addresses the report carried.
    #[must_use]
    pub const fn addresses(&self) -> usize {
        self.addresses
    }

    /// How many of those addresses DWARF named.
    #[must_use]
    pub const fn resolved(&self) -> usize {
        self.resolved
    }

    /// Every distinct address in first-seen order.
    #[must_use]
    pub fn frames(&self) -> &[SymbolizedFrame] {
        &self.frames
    }
}

/// Why a sample report could not be symbolized.
#[derive(Debug)]
pub enum SampleError {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    TooLarge {
        path: PathBuf,
        bytes: u64,
    },
    NotUtf8(PathBuf),
    Shape {
        path: PathBuf,
        reason: &'static str,
    },
    TooManyAddresses {
        path: PathBuf,
        observed: usize,
    },
    Uuid {
        sampled: String,
        installed: String,
    },
    NoTextSegment(String),
    Lookup {
        address: String,
        source: LookupError,
    },
}

impl fmt::Display for SampleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "failed to read sample {}: {source}",
                    path.display()
                )
            }
            Self::TooLarge { path, bytes } => write!(
                formatter,
                "sample {} is {bytes} bytes; the symbolizer reads at most {MAX_SAMPLE_BYTES}",
                path.display()
            ),
            Self::NotUtf8(path) => write!(formatter, "sample {} is not UTF-8", path.display()),
            Self::Shape { path, reason } => {
                write!(formatter, "sample {} {reason}", path.display())
            }
            Self::TooManyAddresses { path, observed } => write!(
                formatter,
                "sample {} carries {observed} distinct addresses; at most \
                 {MAX_SAMPLE_ADDRESSES} are resolved, so take a shorter sample",
                path.display()
            ),
            Self::Uuid { sampled, installed } => write!(
                formatter,
                "sample was taken from image {sampled}; the installed executable is {installed}"
            ),
            Self::NoTextSegment(path) => write!(
                formatter,
                "installed executable {path} carries no __TEXT segment to slide against"
            ),
            Self::Lookup { address, source } => {
                write!(formatter, "lookup of {address} failed: {source}")
            }
        }
    }
}

impl Error for SampleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Lookup { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Resolve every installed-image address of `sample` through the verified DWARF.
///
/// # Errors
///
/// The report is unreadable, oversized, malformed, from another image, or
/// carries more distinct addresses than the bound; or one lookup failed.
pub fn symbolize_sample(
    repository: &Repository,
    verification: &SymbolVerification,
    sample: &Path,
    lookup: &dyn DwarfLookup,
) -> Result<SampleSymbolization, SampleError> {
    let text = read_sample(sample)?;
    let target = SampleTarget::from_verification(repository, verification);
    resolve(sample, &text, &target, lookup)
}

/// The installed image one report is resolved against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SampleTarget {
    image: String,
    uuid: String,
    executable: String,
    text_address: Option<u64>,
    dwarf: PathBuf,
}

impl SampleTarget {
    /// The verified installed pair, as `spice install symbols` resolves against.
    #[must_use]
    pub fn from_verification(repository: &Repository, verification: &SymbolVerification) -> Self {
        Self {
            image: file_name(verification.executable()),
            uuid: verification.uuid().to_owned(),
            executable: verification.executable().to_owned(),
            text_address: verification.text_address(),
            dwarf: verification.dwarf_path(repository),
        }
    }

    /// Canonical `LC_UUID` a report must have been taken from.
    #[must_use]
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// Linked `__TEXT` address runtime addresses slide from.
    #[must_use]
    pub const fn text_address(&self) -> Option<u64> {
        self.text_address
    }

    /// Resolve one already-read report against this target.
    ///
    /// # Errors
    ///
    /// The report is malformed, from another image, over the address bound,
    /// or one lookup failed.
    pub fn resolve(
        &self,
        sample: &Path,
        text: &str,
        lookup: &dyn DwarfLookup,
    ) -> Result<SampleSymbolization, SampleError> {
        resolve(sample, text, self, lookup)
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned()
}

fn resolve(
    sample: &Path,
    text: &str,
    target: &SampleTarget,
    lookup: &dyn DwarfLookup,
) -> Result<SampleSymbolization, SampleError> {
    let parsed = parse_sample(text, &target.image).map_err(|reason| SampleError::Shape {
        path: sample.to_path_buf(),
        reason,
    })?;
    if parsed.addresses.len() > MAX_SAMPLE_ADDRESSES {
        return Err(SampleError::TooManyAddresses {
            path: sample.to_path_buf(),
            observed: parsed.addresses.len(),
        });
    }
    if parsed.uuid != target.uuid {
        return Err(SampleError::Uuid {
            sampled: parsed.uuid,
            installed: target.uuid.clone(),
        });
    }
    let text_address = target
        .text_address
        .ok_or_else(|| SampleError::NoTextSegment(target.executable.clone()))?;
    let mut frames = Vec::with_capacity(parsed.addresses.len());
    for (address, samples) in &parsed.addresses {
        let file_address = address
            .checked_sub(parsed.load_address)
            .and_then(|offset| offset.checked_add(text_address));
        let frame = match file_address {
            Some(file_address) => lookup
                .lookup(&target.dwarf, file_address)
                .map_err(|source| SampleError::Lookup {
                    address: format!("{address:#x}"),
                    source,
                })
                .map(|report| parse_lookup(&report))?,
            None => None,
        };
        frames.push(SymbolizedFrame {
            address: format!("{address:#x}"),
            file_address: file_address.map_or_else(|| "unmapped".to_owned(), |a| format!("{a:#x}")),
            samples: *samples,
            frame,
        });
    }
    Ok(SampleSymbolization {
        sample: sample.display().to_string(),
        load_address: format!("{:#x}", parsed.load_address),
        uuid: parsed.uuid,
        addresses: frames.len(),
        resolved: frames.iter().filter(|frame| frame.frame.is_some()).count(),
        frames,
    })
}

fn read_sample(path: &Path) -> Result<String, SampleError> {
    let metadata = fs::metadata(path).map_err(|source| SampleError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > MAX_SAMPLE_BYTES {
        return Err(SampleError::TooLarge {
            path: path.to_path_buf(),
            bytes: metadata.len(),
        });
    }
    let bytes = fs::read(path).map_err(|source| SampleError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if bytes.len() as u64 > MAX_SAMPLE_BYTES {
        return Err(SampleError::TooLarge {
            path: path.to_path_buf(),
            bytes: bytes.len() as u64,
        });
    }
    String::from_utf8(bytes).map_err(|_| SampleError::NotUtf8(path.to_path_buf()))
}

/// The parts of one `sample(1)` report the symbolizer consumes.
#[derive(Debug, Eq, PartialEq)]
struct ParsedSample {
    load_address: u64,
    uuid: String,
    /// Distinct addresses in first-seen order with their deepest sample count.
    addresses: Vec<(u64, u64)>,
}

fn parse_sample(text: &str, image: &str) -> Result<ParsedSample, &'static str> {
    let load_address = text
        .lines()
        .find_map(|line| line.strip_prefix("Load Address:"))
        .ok_or("names no load address")
        .and_then(|rest| parse_hex(rest.trim()).ok_or("load address is not hexadecimal"))?;
    let uuid = image_uuid(text, image, load_address)?;
    let marker = format!("(in {image})");
    let mut graph = text.lines().skip_while(|line| line.trim() != "Call graph:");
    if graph.next().is_none() {
        return Err("carries no call graph");
    }
    let mut addresses: Vec<(u64, u64)> = Vec::new();
    for line in graph
        .take_while(|line| !line.trim().is_empty())
        .filter(|line| line.contains(&marker))
    {
        let (address, samples) = parse_frame(line).ok_or("carries a malformed frame line")?;
        match addresses.iter_mut().find(|(seen, _)| *seen == address) {
            Some((_, deepest)) => *deepest = (*deepest).max(samples),
            None => addresses.push((address, samples)),
        }
    }
    if addresses.is_empty() {
        return Err("attributes no frame to the installed image");
    }
    Ok(ParsedSample {
        load_address,
        uuid,
        addresses,
    })
}

fn image_uuid(text: &str, image: &str, load_address: u64) -> Result<String, &'static str> {
    let expected = format!("+{image}");
    let mut lines = text
        .lines()
        .skip_while(|line| line.trim() != "Binary Images:");
    if lines.next().is_none() {
        return Err("lists no binary images");
    }
    for line in lines.take_while(|line| !line.trim().is_empty()) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let [start, "-", _, name, rest @ ..] = tokens.as_slice() else {
            continue;
        };
        if *name != expected {
            continue;
        }
        if parse_hex(start) != Some(load_address) {
            return Err("lists the installed image at another address than it loaded");
        }
        let uuid = rest
            .iter()
            .find_map(|token| token.strip_prefix('<')?.strip_suffix('>'))
            .ok_or("lists the installed image without a UUID")?;
        if !macho_uuid_is_canonical(uuid) {
            return Err("lists a non-canonical UUID for the installed image");
        }
        return Ok(uuid.to_owned());
    }
    Err("lists no binary image for the installed executable")
}

/// The runtime address and sample count of one call-tree frame line.
///
/// `sample(1)` resolves a frame itself when it can already find symbols for
/// the sampled image -- exactly what happens once a dSYM sits at this
/// installer's own retained path, since the system symbolizer discovers it
/// there too. A resolved frame keeps its bracketed runtime address but
/// appends the function name before it and a source location after it, so
/// the address is read from the first `[...]` following the last `[`
/// instead of requiring it to end the line.
fn parse_frame(line: &str) -> Option<(u64, u64)> {
    let (_, tail) = line.rsplit_once('[')?;
    let (address, _) = tail.split_once(']')?;
    let address = parse_hex(address.trim())?;
    let samples = line
        .split_whitespace()
        .find_map(|token| token.parse::<u64>().ok())?;
    Some((address, samples))
}

fn parse_hex(token: &str) -> Option<u64> {
    let digits = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))?;
    u64::from_str_radix(digits, 16).ok()
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Die {
    Other,
    Namespace,
    Subprogram,
}

/// The innermost named subprogram of one `dwarfdump --lookup --show-parents` report.
fn parse_lookup(report: &str) -> Option<SampledFrame> {
    let mut namespaces: Vec<String> = Vec::new();
    let mut die = Die::Other;
    let mut function = None;
    let mut file = None;
    let mut line = None;
    let mut line_info = None;
    for raw in report.lines() {
        let text = raw.trim();
        let attribute = text.split_once('\t');
        match (text, attribute) {
            (_, _) if text.ends_with("DW_TAG_namespace") => die = Die::Namespace,
            (_, _) if text.ends_with("DW_TAG_subprogram") => {
                die = Die::Subprogram;
                function = None;
                file = None;
                line = None;
            }
            (_, _) if text.starts_with("0x") || text.starts_with("DW_TAG_") => die = Die::Other,
            (_, Some(("DW_AT_name", value))) => match die {
                Die::Namespace => namespaces.push(quoted(value)?.to_owned()),
                Die::Subprogram => {
                    function = Some(
                        namespaces
                            .iter()
                            .map(String::as_str)
                            .chain([quoted(value)?])
                            .collect::<Vec<_>>()
                            .join("::"),
                    );
                }
                Die::Other => {}
            },
            (_, Some(("DW_AT_decl_file", value))) if die == Die::Subprogram => {
                file = Some(quoted(value)?.to_owned());
            }
            (_, Some(("DW_AT_decl_line", value))) if die == Die::Subprogram => {
                line = Some(parenthesized(value)?.parse().ok()?);
            }
            _ => {
                if let Some(rest) = text.strip_prefix("Line info:") {
                    line_info = parse_line_info(rest);
                }
            }
        }
    }
    Some(SampledFrame {
        function: function?,
        declared: SourceLocation {
            file: file?,
            line: line?,
            column: None,
        },
        line: line_info,
    })
}

/// `file 'X', line N, column C, start file ...` from the line-table report.
fn parse_line_info(rest: &str) -> Option<SourceLocation> {
    let (_, after_file) = rest.split_once("file '")?;
    let (file, after) = after_file.split_once("', line ")?;
    let (line, after) = after.split_once(", column ")?;
    let line: u64 = line.trim().parse().ok()?;
    let column: u64 = after.split(',').next()?.trim().parse().ok()?;
    (line > 0).then(|| SourceLocation {
        file: file.to_owned(),
        line,
        column: (column > 0).then_some(column),
    })
}

fn quoted(value: &str) -> Option<&str> {
    value.trim().strip_prefix("(\"")?.strip_suffix("\")")
}

fn parenthesized(value: &str) -> Option<&str> {
    value.trim().strip_prefix('(')?.strip_suffix(')')
}

#[cfg(test)]
mod tests;
