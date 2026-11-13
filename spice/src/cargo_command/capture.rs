//! Failure-focused rendering of one developer Cargo process.
//!
//! The child runs with the caller's exact arguments, environment, and exit
//! status. Its standard error, and its standard output unless that output is
//! the command's product, reach only one complete capture retained by digest
//! below the Git common directory; delivered product output is copied there
//! too. A person sees a heartbeat naming Cargo's current step or a live demo's
//! serving phase, one completion line with counts and timing, and on failure a
//! bounded tail with truncation facts and an exact inspection command.
//! `--verbose` adds bounded live lines.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spice::shell_quote;
use wire::{Digest, Domain};

use crate::gate_command::output::{
    Captures, Elapsed, Progress, TAIL_BYTES, TAIL_LINES, clip, publish_capture,
};

const APP_DEMO_READY: &str = "[app-demo] endpoint=";

/// Where progress and the completion report are written.
pub(super) type Sink = Arc<Mutex<dyn Write + Send>>;

/// Bytes one pump moves per read.
const READ_BUFFER: usize = 64 * 1024;

/// Bytes of one output line kept for recognizing progress and counts.
const LINE_PREFIX: usize = 1_024;

/// Longest step label a heartbeat carries.
const PHASE_CHARS: usize = 96;

/// Longest command identity a completion line carries.
const IDENTITY_CHARS: usize = 160;

/// Live lines `--verbose` prints before it stops streaming.
const LIVE_LINES: usize = 200;

/// Longest live line `--verbose` prints.
const LIVE_CHARS: usize = 240;

/// How long output may stay open after the child exits, held by a
/// descendant, before reading stops.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Bytes read from the end of the capture to find a failure's tail.
const TAIL_WINDOW: u64 = 2 * TAIL_BYTES as u64;

/// Where the child's standard output goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stdout {
    /// Diagnostic chatter: retained in the capture only.
    Capture,
    /// The command's product: delivered to the caller byte for byte and
    /// copied into the capture.
    Deliver,
}

/// Cargo subcommands whose standard output is their product.
const DELIVERED_SUBCOMMANDS: [&str; 13] = [
    "run",
    "r",
    "metadata",
    "tree",
    "pkgid",
    "locate-project",
    "read-manifest",
    "verify-project",
    "version",
    "help",
    "config",
    "search",
    "info",
];

/// Cargo options before the subcommand that consume the following word.
const VALUE_OPTIONS: [&str; 5] = ["--config", "-Z", "-C", "--color", "--manifest-path"];

/// Subcommands whose test harness lists targets with `-- --list`.
const LISTING_SUBCOMMANDS: [&str; 3] = ["test", "t", "bench"];

/// Whether Cargo's standard output is the product of `arguments`: data
/// subcommands, help and version requests, JSON message formats, and test
/// listings are delivered; build, check, test, lint, and unknown commands
/// are captured.
pub(super) fn cargo_stdout(arguments: &[OsString]) -> Stdout {
    let mut words = arguments.iter().map(|argument| argument.to_string_lossy());
    let subcommand = loop {
        let Some(word) = words.next() else {
            return Stdout::Deliver;
        };
        match word.as_ref() {
            "-h" | "--help" | "-V" | "--version" | "--list" => return Stdout::Deliver,
            option if VALUE_OPTIONS.contains(&option) => {
                words.next();
            }
            option if option.starts_with(['-', '+']) => {}
            _ => break word,
        }
    };
    if DELIVERED_SUBCOMMANDS.contains(&subcommand.as_ref()) {
        return Stdout::Deliver;
    }
    let lists = LISTING_SUBCOMMANDS.contains(&subcommand.as_ref());
    let mut harness = false;
    let mut format_value = false;
    for word in words {
        let word = word.as_ref();
        if harness {
            if lists && word == "--list" {
                return Stdout::Deliver;
            }
        } else if format_value && word.contains("json") {
            return Stdout::Deliver;
        } else if word == "--" {
            harness = true;
        } else if matches!(word, "-h" | "--help")
            || (word.starts_with("--message-format=") && word.contains("json"))
        {
            return Stdout::Deliver;
        }
        format_value = word == "--message-format";
    }
    Stdout::Capture
}

/// The developer Cargo captures below `common_git`'s state root.
pub(super) fn captures(common_git: &Path) -> Captures {
    Captures::in_state(common_git, "cargo")
}

/// `program` and its arguments as a completion line names them.
pub(super) fn identity(program: &str, arguments: &[OsString]) -> String {
    let mut text = program.to_owned();
    for argument in arguments {
        let argument = argument.to_string_lossy();
        text.push(' ');
        if argument.is_empty() || argument.contains(char::is_whitespace) {
            text.push('\'');
            text.push_str(&argument);
            text.push('\'');
        } else {
            text.push_str(&argument);
        }
    }
    clip(&sanitize(&text), IDENTITY_CHARS)
}

/// How one captured run reports.
pub(super) struct Settings {
    /// The surface prefix of every line: `dev cargo` or `dev xtask`.
    pub(super) surface: &'static str,
    /// What ran, as the completion line names it.
    pub(super) identity: String,
    /// Where progress and the completion report are written.
    pub(super) sink: Sink,
    /// Interval between heartbeats.
    pub(super) heartbeat: Duration,
    /// Whether recognized output streams live, bounded.
    pub(super) verbose: bool,
    /// Where the complete capture is retained.
    pub(super) captures: Captures,
}

/// Which step of a captured run failed before the child's status existed.
#[derive(Debug)]
pub(super) enum Stage {
    Log(io::Error),
    Spawn(io::Error),
    Relay(io::Error),
    Wait(io::Error),
}

/// Run `command` with its output captured, report it, and return its status.
///
/// `relay` runs once the child exists and its guard lives until the child
/// exits; a relay failure kills the child.
pub(super) fn run<R>(
    command: &mut Command,
    stdout: Stdout,
    settings: &Settings,
    relay: impl FnOnce(&Child) -> io::Result<R>,
) -> Result<ExitStatus, Stage> {
    let directory = settings.captures.directory();
    fs::create_dir_all(directory).map_err(Stage::Log)?;
    let staged = directory.join(format!(
        ".{}-{}.partial",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let log = File::options()
        .create_new(true)
        .read(true)
        .append(true)
        .open(&staged)
        .map_err(Stage::Log)?;
    let shared = Arc::new(Shared {
        log: Mutex::new(Some(log)),
        tally: Mutex::new(Tally {
            phase: settings.identity.clone(),
            ..Tally::default()
        }),
    });
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    let mut child =
        sill::spawn_owned_command(command, sill::ProcessGroup::Inherit).map_err(|error| {
            let _ = fs::remove_file(&staged);
            Stage::Spawn(error)
        })?;
    #[cfg(not(windows))]
    let mut child = command.spawn().map_err(|error| {
        let _ = fs::remove_file(&staged);
        Stage::Spawn(error)
    })?;
    let guard = match relay(&child) {
        Ok(guard) => guard,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&staged);
            return Err(Stage::Relay(error));
        }
    };

    let (done, drained) = mpsc::channel();
    let live = || {
        settings
            .verbose
            .then(|| Progress::new(settings.surface, settings.sink.clone(), settings.heartbeat))
    };
    let mut pumps = 0;
    if let Some(source) = child.stdout.take() {
        let deliver = (stdout == Stdout::Deliver).then(io::stdout);
        let pump = Pump::new(&shared, live(), &done);
        std::thread::spawn(move || pump.run(source, deliver));
        pumps += 1;
    }
    if let Some(source) = child.stderr.take() {
        let pump = Pump::new(&shared, live(), &done);
        std::thread::spawn(move || pump.run(source, None));
        pumps += 1;
    }
    drop(done);

    let progress = Progress::new(settings.surface, settings.sink.clone(), settings.heartbeat);
    let (status, elapsed) = progress.watching(|| shared.phase(), || child.wait());
    drop(guard);
    let status = status.map_err(|error| {
        let _ = fs::remove_file(&staged);
        Stage::Wait(error)
    })?;

    let deadline = Instant::now() + DRAIN_GRACE;
    while pumps > 0
        && drained
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    {
        pumps -= 1;
    }
    let log = shared.log.lock().ok().and_then(|mut log| log.take());
    let tally = shared
        .tally
        .lock()
        .map(|tally| tally.clone())
        .unwrap_or_default();
    let report = Report {
        status,
        elapsed,
        tally: &tally,
        abandoned: pumps > 0,
    };
    let text = report.render(settings, log, &staged);
    emit(&settings.sink, &text);
    Ok(status)
}

/// State the pumps share with the reporter.
struct Shared {
    /// The capture; taken at report time so a straggling write is dropped.
    log: Mutex<Option<File>>,
    tally: Mutex<Tally>,
}

impl Shared {
    fn phase(&self) -> String {
        self.tally
            .lock()
            .map_or_else(|_| "running".to_owned(), |tally| tally.phase.clone())
    }
}

/// What a run's output said, counted as it streamed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Tally {
    bytes: u64,
    lines: u64,
    suites: u64,
    passed: u64,
    failed: u64,
    ignored: u64,
    compiled: u64,
    warnings: u64,
    phase: String,
    live: usize,
    lost: bool,
}

impl Tally {
    /// Count one complete output line.
    fn observe(&mut self, line: &str) {
        self.lines += 1;
        let trimmed = line.trim_start();
        if let Some(endpoint) = trimmed.strip_prefix(APP_DEMO_READY) {
            self.phase = clip(&format!("app-demo serving {endpoint}"), PHASE_CHARS);
            return;
        }
        if let Some(result) = trimmed.strip_prefix("test result: ") {
            self.suites += 1;
            for part in result.split([';', '.']) {
                let mut words = part.split_whitespace();
                let (Some(count), Some(label)) = (words.next(), words.next()) else {
                    continue;
                };
                let Ok(count) = count.parse::<u64>() else {
                    continue;
                };
                match label {
                    "passed" => self.passed += count,
                    "failed" => self.failed += count,
                    "ignored" => self.ignored += count,
                    _ => {}
                }
            }
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("warning: ")
            && let Some((_, count)) = rest.split_once(" generated ")
            && let Some(count) = count
                .split_whitespace()
                .next()
                .and_then(|count| count.parse::<u64>().ok())
        {
            self.warnings += count;
            return;
        }
        if line.starts_with(' ')
            && let Some((verb, _)) = trimmed.split_once(' ')
            && verb.starts_with(|first: char| first.is_ascii_uppercase())
            && verb
                .chars()
                .all(|letter| letter.is_ascii_alphabetic() || letter == '-')
        {
            if matches!(verb, "Compiling" | "Checking") {
                self.compiled += 1;
            }
            self.phase = clip(trimmed, PHASE_CHARS);
        }
    }

    fn counts(&self) -> String {
        let mut parts = Vec::new();
        if self.suites > 0 {
            parts.push(format!(
                "tests {} passed, {} failed, {} ignored in {}",
                self.passed,
                self.failed,
                self.ignored,
                counted(self.suites, "suite")
            ));
        }
        if self.compiled > 0 {
            parts.push(format!("{} compiled", counted(self.compiled, "crate")));
        }
        if self.warnings > 0 {
            parts.push(counted(self.warnings, "warning"));
        }
        parts.join("; ")
    }
}

/// `count noun`, plural unless the count is one.
fn counted(count: u64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// One output stream's copy into the capture.
struct Pump {
    shared: Arc<Shared>,
    live: Option<Progress>,
    done: mpsc::Sender<()>,
}

impl Pump {
    fn new(shared: &Arc<Shared>, live: Option<Progress>, done: &mpsc::Sender<()>) -> Self {
        Self {
            shared: Arc::clone(shared),
            live,
            done: done.clone(),
        }
    }

    fn run(self, mut source: impl Read, mut deliver: Option<io::Stdout>) {
        let mut buffer = vec![0_u8; READ_BUFFER];
        let mut line = Vec::with_capacity(LINE_PREFIX);
        loop {
            let read = match source.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let chunk = &buffer[..read];
            let written = self.shared.log.lock().ok().is_some_and(|mut log| {
                log.as_mut()
                    .is_some_and(|file| file.write_all(chunk).is_ok())
            });
            if let Some(out) = deliver.as_mut()
                && out.write_all(chunk).and_then(|()| out.flush()).is_err()
            {
                deliver = None;
            }
            if let Ok(mut tally) = self.shared.tally.lock() {
                tally.bytes += read as u64;
                tally.lost |= !written;
            }
            for piece in chunk.split_inclusive(|byte| *byte == b'\n') {
                let (content, ends) = match piece.split_last() {
                    Some((b'\n', content)) => (content, true),
                    _ => (piece, false),
                };
                let room = LINE_PREFIX - line.len();
                line.extend_from_slice(&content[..content.len().min(room)]);
                if ends {
                    self.recognize(&line);
                    line.clear();
                }
            }
        }
        if !line.is_empty() {
            self.recognize(&line);
        }
        let _ = self.done.send(());
    }

    fn recognize(&self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        let Ok(mut tally) = self.shared.tally.lock() else {
            return;
        };
        tally.observe(&text);
        let Some(live) = &self.live else {
            return;
        };
        let shown = tally.live;
        tally.live += 1;
        drop(tally);
        if shown < LIVE_LINES {
            live.line(format_args!("{}", clip(&sanitize(&text), LIVE_CHARS)));
        } else if shown == LIVE_LINES {
            live.line(format_args!(
                "live output stops after {LIVE_LINES} lines; the complete output is retained"
            ));
        }
    }
}

/// The end of a capture a person reads after a failure.
#[derive(Debug, PartialEq, Eq)]
struct Tail {
    /// Terminal-safe kept text, ending at the capture's end.
    text: String,
    /// Lines kept.
    shown_lines: usize,
    /// Capture bytes before the kept text.
    withheld_bytes: u64,
}

impl Tail {
    /// Keep at most the last [`TAIL_LINES`] lines in [`TAIL_BYTES`] of a
    /// `total`-byte capture, reading only its final window.
    fn of(source: &mut (impl Read + Seek), total: u64) -> io::Result<Self> {
        let start = total.saturating_sub(TAIL_WINDOW);
        source.seek(SeekFrom::Start(start))?;
        let mut window = Vec::new();
        source.take(TAIL_WINDOW).read_to_end(&mut window)?;
        let mut body = window.as_slice();
        if start > 0
            && let Some(newline) = body.iter().position(|byte| *byte == b'\n')
        {
            body = &body[newline + 1..];
        }
        let trimmed = body.strip_suffix(b"\n").unwrap_or(body);
        let mut kept = 0;
        let mut shown_lines = 0;
        if !trimmed.is_empty() {
            for line in trimmed.rsplit(|byte| *byte == b'\n') {
                let cost = line.len() + usize::from(shown_lines > 0);
                if shown_lines == TAIL_LINES || kept + cost > TAIL_BYTES {
                    if shown_lines == 0 {
                        kept = TAIL_BYTES;
                        shown_lines = 1;
                    }
                    break;
                }
                kept += cost;
                shown_lines += 1;
            }
        }
        let shown = &trimmed[trimmed.len() - kept..];
        let suffix = body.len() - trimmed.len();
        Ok(Self {
            text: sanitize(&String::from_utf8_lossy(shown)),
            shown_lines,
            withheld_bytes: total.saturating_sub((kept + suffix) as u64),
        })
    }
}

struct Report<'a> {
    status: ExitStatus,
    elapsed: Duration,
    tally: &'a Tally,
    abandoned: bool,
}

impl Report<'_> {
    fn render(&self, settings: &Settings, log: Option<File>, staged: &Path) -> String {
        let mut headline = format!(
            "[{}] {} {} in {}",
            settings.surface,
            settings.identity,
            disposition(self.status),
            Elapsed(self.elapsed)
        );
        let counts = self.tally.counts();
        if !counts.is_empty() {
            headline.push_str("; ");
            headline.push_str(&counts);
        }
        let Some(mut log) = log else {
            return format!("{headline}\ncomplete output was not retained: capture unavailable");
        };
        let tail = (!self.status.success()).then(|| Tail::of(&mut log, self.tally.bytes));
        let retained = retain(&settings.captures, staged, &mut log);
        let mut text = headline;
        match &retained {
            Ok((path, _)) if self.status.success() => {
                text.push_str(&format!("; log {}", path.display()));
            }
            Ok(_) => {}
            Err(error) => {
                text.push_str(&format!("\ncomplete output was not retained: {error}"));
            }
        }
        if let Some(tail) = tail {
            match tail {
                Ok(tail) if tail.shown_lines == 0 => text.push_str("\n(no output)"),
                Ok(tail) => text.push_str(&format!(
                    "\nlast {} of {} lines ({} earlier bytes withheld):\n{}",
                    tail.shown_lines, self.tally.lines, tail.withheld_bytes, tail.text
                )),
                Err(error) => text.push_str(&format!("\noutput tail unreadable: {error}")),
            }
            if let Ok((path, bytes)) = &retained {
                text.push_str(&format!(
                    "\ncomplete output: {} ({bytes} bytes)",
                    path.display()
                ));
                if let Some(path) = path.to_str() {
                    text.push_str(&format!(
                        "\ninspect: grep -n -E 'error|warning|FAILED|panicked' {}",
                        shell_quote(path)
                    ));
                }
            }
        }
        if self.tally.lost {
            text.push_str("\nsome output could not be written to the capture");
        }
        if self.abandoned {
            text.push_str("\na descendant still held the output open; reading stopped at exit");
        }
        text
    }
}

/// Retain the staged capture as `DIGEST.log` and prune its predecessors.
fn retain(captures: &Captures, staged: &Path, log: &mut File) -> Result<(PathBuf, u64), String> {
    log.seek(SeekFrom::Start(0))
        .map_err(|error| format!("{}: {error}", staged.display()))?;
    let (bytes, digest) = Digest::compute_reader(Domain::CliExport, &mut *log, u64::MAX)
        .map_err(|error| format!("{}: {error}", staged.display()))?;
    let path = captures.directory().join(format!("{digest}.log"));
    publish_capture(captures, || {
        fs::rename(staged, &path).map_err(|error| format!("{}: {error}", staged.display()))?;
        Ok((path.clone(), (path, bytes)))
    })
}

fn disposition(status: ExitStatus) -> String {
    if status.success() {
        return "ok".to_owned();
    }
    if let Some(code) = status.code() {
        return format!("failed (exit {code})");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return signal_hook::low_level::signal_name(signal).map_or_else(
                || format!("terminated by signal {signal}"),
                |name| format!("terminated by {name}"),
            );
        }
    }
    "failed".to_owned()
}

/// `text` with every control character but newline and tab replaced, so
/// captured escape sequences never reach the terminal raw.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\t') {
                char::REPLACEMENT_CHARACTER
            } else {
                character
            }
        })
        .collect()
}

fn emit(sink: &Sink, text: &str) {
    if let Ok(mut sink) = sink.lock() {
        let _ = writeln!(sink, "{text}").and_then(|()| sink.flush());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_demo_readiness_replaces_the_completed_build_phase() {
        let mut tally = Tally::default();
        tally.observe("    Finished `dev` profile in 1.2s");
        assert!(tally.phase.starts_with("Finished "));

        tally.observe("[app-demo] endpoint=http://127.0.0.1:43123");

        assert_eq!(tally.phase, "app-demo serving http://127.0.0.1:43123");
        assert!(!tally.phase.contains("Finished"));
        assert!(!tally.phase.contains("build"));
        assert!(!tally.phase.contains("cargo"));
    }
}
