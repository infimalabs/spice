//! Persisted final-link commands for rack-only executable updates.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

/// Cargo's answer for one native binary build, plus the final link it emitted
/// when rustc actually rebuilt that binary.
pub(crate) struct CargoLink {
    pub(crate) executable: PathBuf,
    pub(crate) emitted: Option<LinkPlan>,
    pub(crate) artifacts: usize,
    pub(crate) rebuilt: usize,
}

/// One final linker invocation emitted by rustc's stable `--print link-args`.
///
/// The raw line is retained so every argument and environment byte rustc
/// selected remains the authority across a rack-only relink. Production builds
/// execute on Unix (including the Windows cross-link), where rustc renders this
/// as one shell-escaped command line.
pub(crate) struct LinkPlan {
    raw: String,
    program: PathBuf,
    arguments: Vec<OsString>,
    set: Vec<(OsString, OsString)>,
    unset: Vec<OsString>,
    output: PathBuf,
}

impl LinkPlan {
    /// Parse the one final-link command for `canvas_object`.
    fn parse(line: &str, canvas_object: &Path) -> Result<Self, LinkPlanError> {
        let words =
            shell_words::split(line).map_err(|error| LinkPlanError::Syntax(error.to_string()))?;
        let mut at = 0;
        let mut set = Vec::new();
        let mut unset = Vec::new();
        if words.first().map(String::as_str) == Some("env") {
            at += 1;
        }
        while at < words.len() {
            if words[at] == "-u" {
                let key = words
                    .get(at + 1)
                    .ok_or(LinkPlanError::Shape("env -u has no key"))?;
                unset.push(OsString::from(key));
                at += 2;
                continue;
            }
            if let Some((key, value)) = words[at].split_once('=') {
                if key.is_empty() {
                    return Err(LinkPlanError::Shape("environment key is empty"));
                }
                set.push((OsString::from(key), OsString::from(value)));
                at += 1;
                continue;
            }
            break;
        }
        let program = words
            .get(at)
            .filter(|word| !word.is_empty())
            .map(PathBuf::from)
            .ok_or(LinkPlanError::Shape("link command has no program"))?;
        at += 1;
        let mut arguments = words[at..].iter().map(OsString::from).collect::<Vec<_>>();
        let expected = canvas_object
            .to_str()
            .ok_or(LinkPlanError::Shape("canvas object path is not UTF-8"))?;
        if !words[at..].iter().any(|word| word == expected) {
            return Err(LinkPlanError::Shape(
                "link command does not name the canvas object",
            ));
        }
        let output = output_path(&arguments)?;
        arguments.retain(|argument| !temporary_symbols(argument));
        Ok(Self {
            raw: line.to_owned(),
            program,
            arguments,
            set,
            unset,
            output,
        })
    }

    /// Read and re-admit a previously emitted plan.
    pub(crate) fn read(path: &Path, canvas_object: &Path) -> Result<Self, LinkPlanError> {
        let raw = fs::read_to_string(path).map_err(|source| LinkPlanError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(raw.trim_end_matches(['\n', '\r']), canvas_object)
    }

    /// Publish a complete plan beside the stable canvas object.
    pub(crate) fn publish(&self, path: &Path) -> Result<(), LinkPlanError> {
        let destination = atomic::Destination::new(path)
            .map_err(|error| LinkPlanError::Atomic(error.to_string()))?;
        let mut staged = destination
            .stage()
            .map_err(|error| LinkPlanError::Atomic(error.to_string()))?;
        staged
            .write_all(self.raw.as_bytes())
            .and_then(|()| staged.write_all(b"\n"))
            .map_err(|source| LinkPlanError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        staged
            .commit()
            .map_err(|error| LinkPlanError::Atomic(error.to_string()))
    }

    pub(crate) fn program(&self) -> &Path {
        &self.program
    }

    pub(crate) fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    pub(crate) fn environment(&self) -> (&[(OsString, OsString)], &[OsString]) {
        (&self.set, &self.unset)
    }

    /// Execute this plan in the current process environment.
    pub(crate) fn execute(&self) -> Result<ExitStatus, LinkPlanError> {
        let mut command = Command::new(&self.program);
        command.args(&self.arguments);
        for key in &self.unset {
            command.env_remove(key);
        }
        for (key, value) in &self.set {
            command.env(key, value);
        }
        command.status().map_err(|source| LinkPlanError::Io {
            path: self.program.clone(),
            source,
        })
    }

    /// Copy the linker's hashed Cargo output to Cargo's public executable path.
    pub(crate) fn publish_output(&self, executable: &Path) -> Result<(), LinkPlanError> {
        copy_if_distinct(&self.output, executable)?;
        let pdb = self.output.with_extension("pdb");
        if pdb.is_file() {
            copy_if_distinct(&pdb, &executable.with_extension("pdb"))?;
        }
        Ok(())
    }
}

/// Parse Cargo's JSON event stream and its optional printed final-link command.
pub(crate) fn cargo_output(bytes: &[u8], canvas_object: &Path) -> Result<CargoLink, LinkPlanError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| LinkPlanError::Shape("Cargo output is not UTF-8"))?;
    let mut executables = Vec::new();
    let mut emitted = Vec::new();
    let mut artifacts = 0;
    let mut rebuilt = 0;
    for line in text.lines().filter(|line| !line.is_empty()) {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(value) => {
                if value.get("reason").and_then(serde_json::Value::as_str)
                    == Some("compiler-artifact")
                {
                    artifacts += 1;
                    if value.get("fresh").and_then(serde_json::Value::as_bool) != Some(true) {
                        rebuilt += 1;
                    }
                }
                if value.get("reason").and_then(serde_json::Value::as_str)
                    != Some("compiler-artifact")
                    || value
                        .pointer("/target/name")
                        .and_then(serde_json::Value::as_str)
                        != Some("spice")
                    || !value
                        .pointer("/target/kind")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin")))
                {
                    continue;
                }
                if let Some(executable) =
                    value.get("executable").and_then(serde_json::Value::as_str)
                {
                    executables.push(PathBuf::from(executable));
                }
            }
            Err(_) => emitted.push(LinkPlan::parse(line, canvas_object)?),
        }
    }
    if executables.len() != 1 {
        return Err(LinkPlanError::Population {
            kind: "Spice executables",
            observed: executables.len(),
        });
    }
    if emitted.len() > 1 {
        return Err(LinkPlanError::Population {
            kind: "final link commands",
            observed: emitted.len(),
        });
    }
    Ok(CargoLink {
        executable: executables.pop().expect("one executable exists"),
        emitted: emitted.pop(),
        artifacts,
        rebuilt,
    })
}

/// Apply rustc's link environment to a release executor's sealed environment.
pub(crate) fn overlay_environment(
    base: &sill::Environment,
    set: &[(OsString, OsString)],
    unset: &[OsString],
) -> Result<sill::Environment, sill::SpecError> {
    let mut entries = base.entries().iter().cloned().collect::<BTreeMap<_, _>>();
    for key in unset {
        entries.remove(key);
    }
    for (key, value) in set {
        entries.insert(key.clone(), value.clone());
    }
    sill::Environment::new(entries)
}

fn output_path(arguments: &[OsString]) -> Result<PathBuf, LinkPlanError> {
    let mut outputs = Vec::new();
    for pair in arguments.windows(2) {
        if pair[0] == OsStr::new("-o") {
            outputs.push(PathBuf::from(&pair[1]));
        }
    }
    for argument in arguments {
        let Some(argument) = argument.to_str() else {
            continue;
        };
        if argument.len() > 5 && argument[..5].eq_ignore_ascii_case("/out:") {
            outputs.push(PathBuf::from(&argument[5..]));
        }
    }
    if outputs.len() == 1 {
        Ok(outputs.pop().expect("one linker output exists"))
    } else {
        Err(LinkPlanError::Population {
            kind: "linker outputs",
            observed: outputs.len(),
        })
    }
}

fn temporary_symbols(argument: &OsStr) -> bool {
    let path = Path::new(argument);
    matches!(
        path.file_name().and_then(OsStr::to_str),
        Some("symbols.o" | "symbols.obj")
    ) && path
        .parent()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.starts_with("rustc"))
}

fn copy_if_distinct(source: &Path, destination: &Path) -> Result<(), LinkPlanError> {
    if source == destination {
        return Ok(());
    }
    fs::copy(source, destination)
        .map(|_| ())
        .map_err(|source_error| LinkPlanError::Io {
            path: destination.to_path_buf(),
            source: source_error,
        })
}

#[derive(Debug)]
pub(crate) enum LinkPlanError {
    Atomic(String),
    Io { path: PathBuf, source: io::Error },
    Population { kind: &'static str, observed: usize },
    Shape(&'static str),
    Syntax(String),
}

impl fmt::Display for LinkPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Atomic(error) => write!(formatter, "link plan publication failed: {error}"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Population { kind, observed } => {
                write!(
                    formatter,
                    "observed {observed} {kind}; expected exactly one"
                )
            }
            Self::Shape(detail) => write!(formatter, "malformed final link command: {detail}"),
            Self::Syntax(detail) => write!(formatter, "malformed final link command: {detail}"),
        }
    }
}

impl Error for LinkPlanError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_link_plan_preserves_environment_and_drops_rustc_temporary_symbols() {
        let object = Path::new("/build/canvas/release.macos.o");
        let events = concat!(
            "env -u OLD LC_ALL=\"C\" \"cc\" \"/tmp/rustc123/symbols.o\" ",
            "\"/build/spice.rcgu.o\" \"/build/canvas/release.macos.o\" ",
            "\"-o\" \"/build/deps/spice-hash\"\n",
            "{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"spice\",",
            "\"kind\":[\"bin\"]},\"executable\":\"/build/spice\"}\n",
        );
        let cargo = cargo_output(events.as_bytes(), object).expect("link output admits");
        assert_eq!(cargo.executable, Path::new("/build/spice"));
        assert!(cargo.artifacts > 0);
        assert!(cargo.rebuilt > 0);
        let plan = cargo
            .emitted
            .expect("a rebuilt binary emits its final link");
        assert_eq!(plan.program(), Path::new("cc"));
        assert!(
            !plan
                .arguments()
                .iter()
                .any(|argument| argument == "/tmp/rustc123/symbols.o")
        );
        assert!(plan.arguments().iter().any(|argument| argument == object));
        assert_eq!(
            plan.environment().0,
            [(OsString::from("LC_ALL"), OsString::from("C"))]
        );
        assert_eq!(plan.environment().1, [OsString::from("OLD")]);
    }

    #[test]
    fn a_fresh_binary_needs_no_new_link_command() {
        let events = concat!(
            "{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"spice\",",
            "\"kind\":[\"bin\"]},\"executable\":\"/build/spice\",\"fresh\":true}\n",
            "{\"reason\":\"build-finished\",\"success\":true}\n",
        );
        let cargo = cargo_output(events.as_bytes(), Path::new("/build/canvas/dev.macos.o"))
            .expect("fresh Cargo output admits");
        assert_eq!(cargo.executable, Path::new("/build/spice"));
        assert!(cargo.emitted.is_none());
        assert_eq!(cargo.artifacts, 1);
        assert_eq!(cargo.rebuilt, 0);
    }

    #[test]
    fn a_cross_link_command_without_an_env_prefix_is_admitted() {
        let object = Path::new("/build/canvas/release.windows.obj");
        let events = concat!(
            "\"lld-link\" \"/build/spice.obj\" \"/build/canvas/release.windows.obj\" ",
            "\"/OUT:/build/deps/spice.exe\"\n",
            "{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"spice\",",
            "\"kind\":[\"bin\"]},\"executable\":\"/build/spice.exe\"}\n",
        );
        let cargo = cargo_output(events.as_bytes(), object).expect("cross-link output admits");
        let plan = cargo.emitted.expect("the cross link was captured");
        assert_eq!(plan.program(), Path::new("lld-link"));
    }
}
