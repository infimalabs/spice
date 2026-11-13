use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{AppDemoError, DemoChild, fail};

/// The compiled vendor whose occupant plays a cue script.
const VENDOR: &str = "scripted";
/// Worktree file holding the built-in proof script.
const SCRIPT_FILE: &str = "scripted-proof.cue";
/// The built-in proof script, played at the fastest rate.
const SCRIPT: &str = include_str!("scripted_proof.cue");
/// Reply text the script's last cue carries; its capture proves every cue
/// played.
fn final_reply() -> &'static str {
    SCRIPT
        .lines()
        .rev()
        .find_map(|line| line.split_once(" say ").map(|(_, text)| text))
        .expect("the built-in script ends with a say cue")
}
const TIMEOUT: Duration = Duration::from_secs(45);
const POLL: Duration = Duration::from_millis(50);
/// The operator direction the script's steering cue acknowledges. Only the
/// operator authors steering, so this driver delivers it.
const STEERING: &str = "Acknowledge this scripted native steering through spice://outbox.";

pub(super) struct Harness {
    lane: Option<String>,
    home: PathBuf,
    worktree: PathBuf,
}

impl Harness {
    pub(super) fn prepare(root: &Path) -> Result<Self, AppDemoError> {
        let home = root.join(".scripted-omp-home");
        let worktree = root.join(".scripted-omp-worktree");
        let linked = Command::new("git")
            .current_dir(root)
            .args(["worktree", "add", "--detach"])
            .arg(&worktree)
            .output()
            .map_err(|error| {
                fail(format!(
                    "scripted OMP worktree setup did not start: {error}"
                ))
            })?;
        ensure_success("worktree setup", linked)?;
        fs::create_dir_all(&home)
            .and_then(|()| fs::write(worktree.join(SCRIPT_FILE), SCRIPT))
            .map_err(|error| fail(format!("scripted OMP setup failed: {error}")))?;
        let context = (0..800)
            .map(|line| {
                format!(
                    "bounded scripted OMP compaction context line {line:03}: {}",
                    "x".repeat(96)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(worktree.join("scripted-context.txt"), context).map_err(|error| {
            fail(format!(
                "scripted OMP context fixture write failed: {error}"
            ))
        })?;

        Ok(Self {
            lane: None,
            home,
            worktree,
        })
    }

    pub(super) fn seat(&mut self, executable: &Path, root: &Path) -> Result<(), AppDemoError> {
        let task_output = isolated_command(executable, root, &self.home)
            .args([
                "task",
                "add",
                "Exercise the scripted OMP proof lane",
                "--topic",
                "/demo",
                "--flow",
                "todo",
                "--pitch",
                "A real scripted occupant proves native capture without model inference.",
                "--description",
                "Produce native OMP tool, steering, and compaction records through the disposable resident.",
                "--term",
                "The scripted occupant settles after its native records are captured.",
                "--draft",
                "--json",
            ])
            .output()
            .map_err(|error| fail(format!("scripted OMP draft setup did not start: {error}")))?;
        let draft = command_identity("draft setup", task_output, "task")?;

        let lane_output = isolated_command(executable, root, &self.home)
            .arg("lane")
            .arg("add")
            .arg(&self.worktree)
            .args([
                "--name",
                "Scripted OMP",
                "--life",
                "Drive",
                "--vendor",
                VENDOR,
                "--model",
                &format!("{SCRIPT_FILE}@instant"),
                "--json",
            ])
            .output()
            .map_err(|error| fail(format!("scripted OMP lane setup did not start: {error}")))?;
        let lane = command_identity("lane setup", lane_output, "lane")?;
        let steered = isolated_command(executable, root, &self.home)
            .args(["steer", &lane, STEERING, "--json"])
            .output()
            .map_err(|error| fail(format!("scripted OMP steering did not start: {error}")))?;
        ensure_success("steering", steered)?;
        let filed = isolated_command(executable, root, &self.home)
            .args(["task", "file", &draft, "--json"])
            .output()
            .map_err(|error| fail(format!("scripted OMP task filing did not start: {error}")))?;
        ensure_success("task filing", filed)?;
        let wake = isolated_command(executable, root, &self.home)
            .args(["lane", "wake", &lane, "--json"])
            .output()
            .map_err(|error| fail(format!("scripted OMP lane wake did not start: {error}")))?;
        ensure_wake(wake)?;
        self.lane = Some(lane);
        Ok(())
    }

    pub(super) fn configure(&self, command: &mut Command) {
        command
            .env("HOME", &self.home)
            .env("PI_CODING_AGENT_DIR", self.home.join("agent"));
        for name in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GOOGLE_API_KEY",
            "GEMINI_API_KEY",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            wire::SpiceVariable::Lane.as_str(),
            wire::SpiceVariable::Session.as_str(),
        ] {
            command.env_remove(name);
        }
    }

    pub(super) fn drive(
        &mut self,
        executable: &Path,
        root: &Path,
        child: &mut DemoChild,
    ) -> Result<(), AppDemoError> {
        let lane = self
            .lane
            .as_deref()
            .ok_or_else(|| fail("scripted OMP lane was not seated"))?;
        let deadline = Instant::now() + TIMEOUT;

        // The occupant plays its script; the resident may still be ingesting
        // its tape after the last reply. Once every cue has played, a prompt
        // the occupant receives later, such as its post-compaction briefing,
        // finds the script exhausted; only a refusal before then fails.
        loop {
            let capture = read_capture(root, lane)?;
            let finished = capture.contains(final_reply());
            if !finished && let Some(refused) = cue_refusal(&capture) {
                return Err(fail(format!("scripted OMP refused: {refused}")));
            }
            let missing = match capture_gap(&capture, STEERING) {
                Some(missing) => Some(missing),
                None if !finished => Some("final scripted reply"),
                None => fact_gap(executable, root, &self.home, lane)?,
            };
            let Some(missing) = missing else { break };
            if Instant::now() >= deadline {
                return Err(fail(format!("scripted OMP run omitted its {missing}")));
            }
            thread::sleep(POLL);
        }
        child.interrupt()?;
        Ok(())
    }
}

fn isolated_command(executable: &Path, root: &Path, home: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(root)
        .env("HOME", home)
        .env("PI_CODING_AGENT_DIR", home.join("agent"))
        .env_remove(wire::SpiceVariable::Lane.as_str())
        .env_remove(wire::SpiceVariable::Session.as_str());
    command
}

fn command_identity(stage: &str, output: Output, key: &str) -> Result<String, AppDemoError> {
    if !output.status.success() {
        return Err(command_failure(stage, output));
    }
    let document: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        fail(format!(
            "scripted OMP {stage} returned invalid JSON: {error}"
        ))
    })?;
    find_string(&document, key)
        .map(str::to_owned)
        .ok_or_else(|| fail(format!("scripted OMP {stage} returned no {key} identity")))
}

fn find_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    match value {
        Value::Object(fields) => fields
            .get(key)
            .and_then(Value::as_str)
            .or_else(|| fields.values().find_map(|value| find_string(value, key))),
        Value::Array(values) => values.iter().find_map(|value| find_string(value, key)),
        _ => None,
    }
}

fn ensure_success(stage: &str, output: Output) -> Result<(), AppDemoError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure(stage, output))
    }
}

fn ensure_wake(output: Output) -> Result<(), AppDemoError> {
    if output.status.success()
        || String::from_utf8_lossy(&output.stdout).contains("lifecycle::launch-pending")
        || String::from_utf8_lossy(&output.stderr).contains("lifecycle::launch-pending")
    {
        Ok(())
    } else {
        Err(command_failure("lane wake", output))
    }
}

fn command_failure(stage: &str, output: Output) -> AppDemoError {
    let detail = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    fail(format!(
        "scripted OMP {stage} failed: {}",
        String::from_utf8_lossy(detail).trim()
    ))
}

/// The scripted provider's refusal, when the capture records one: the
/// provider fails the model request with a `scripted cue:` error naming the
/// cue line.
fn cue_refusal(capture: &str) -> Option<&str> {
    const PREFIX: &str = "scripted cue: ";
    let start = capture.find(PREFIX)? + PREFIX.len();
    let rest = &capture[start..];
    Some(&rest[..rest.find('"').unwrap_or(rest.len())])
}

/// The lane's retained OMP stream, every plain member in ordinal order. A
/// lane that has not launched yet has none.
fn read_capture(root: &Path, lane: &str) -> Result<String, AppDemoError> {
    let repository = spice::repository::Repository::discover(root)
        .map_err(|error| fail(format!("scripted OMP root unavailable: {error}")))?;
    let state = repository
        .admit_state()
        .map_err(|error| fail(format!("scripted OMP state unavailable: {error}")))?;
    let lane = lane
        .parse::<wire::CanonicalUuid>()
        .map_err(|error| fail(format!("scripted OMP lane is invalid: {error}")))?;
    let tape_root = state.lane_tape(lane).join(VENDOR);
    let entries = match fs::read_dir(&tape_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => {
            return Err(fail(format!(
                "scripted OMP capture unavailable at {}: {error}",
                tape_root.display()
            )));
        }
    };
    let mut members = entries
        .map(|entry| entry.map_err(|error| fail(format!("scripted OMP capture entry: {error}"))))
        .collect::<Result<Vec<_>, _>>()?;
    members.sort_by_key(|entry| entry.file_name());
    let mut bytes = Vec::new();
    for entry in members {
        let name = entry.file_name();
        let ordinal = name.to_string_lossy();
        if entry
            .file_type()
            .map_err(|error| fail(format!("scripted OMP capture type: {error}")))?
            .is_file()
            && ordinal
                .strip_suffix(".plain")
                .is_some_and(|ordinal| ordinal.bytes().all(|byte| byte.is_ascii_digit()))
        {
            bytes.extend(fs::read(entry.path()).map_err(|error| {
                fail(format!("scripted OMP capture member unavailable: {error}"))
            })?);
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The first native record the capture lacks, in the order the occupant must
/// produce them: the delivered steering, its occupant-authored outbox write,
/// then OMP's own manual compaction completion.
fn capture_gap(capture: &str, steering: &str) -> Option<&'static str> {
    const WITNESSES: [&str; 3] = [
        "steering delivery",
        "outbox host request",
        "manual compaction record",
    ];
    let mut found = 0;
    for line in capture.lines() {
        let kind = || {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|record| record.get("type")?.as_str().map(str::to_owned))
        };
        let matched = match found {
            0 => line.contains(steering),
            1 => line.contains("spice://outbox") && kind().as_deref() == Some("host_uri_request"),
            2 => kind().as_deref() == Some("manual_compaction_end"),
            _ => break,
        };
        if matched {
            found += 1;
        }
    }
    WITNESSES.get(found).copied()
}

/// The first durable resident fact the lane lacks: the steering
/// acknowledgement its outbox write earned, then its compaction boundary.
fn fact_gap(
    executable: &Path,
    root: &Path,
    home: &Path,
    lane: &str,
) -> Result<Option<&'static str>, AppDemoError> {
    let ours = format!("\"lane\":\"{lane}\"");
    let (mut acked, mut compacted) = (false, false);
    let mut after = None;
    while !(acked && compacted) {
        let mut command = isolated_command(executable, root, home);
        command.args(["journal", "--json"]);
        if let Some(after) = after {
            command.arg("--after").arg(format!("{after}"));
        }
        let output = command
            .output()
            .map_err(|error| fail(format!("scripted OMP journal read did not start: {error}")))?;
        let rows = match serde_json::from_slice(&output.stdout) {
            Ok(wire::Outcome::<wire::JournalReport>::Reply(reply)) => reply.body.entries.rows,
            _ => return Err(command_failure("journal read", output)),
        };
        for row in &rows {
            if row.fact.text.contains(&ours) {
                acked |= row.kind == wire::FactKind::SteerAcked;
                compacted |= row.kind == wire::FactKind::LaneCompacted;
            }
        }
        match rows.last().map(|row| row.seq.get() - 1) {
            Some(next) if next > 0 => after = Some(next),
            _ => break,
        }
    }
    Ok(if !acked {
        Some("steering acknowledgement fact")
    } else if !compacted {
        Some("lane compaction fact")
    } else {
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEER: &str = "native steering";

    fn record(kind: &str, extra: &str) -> String {
        format!("{{\"type\":\"{kind}\"{extra}}}")
    }

    #[test]
    fn capture_requires_typed_native_records_in_occupant_order() {
        let steering = record("message_start", &format!(",\"text\":\"{STEER}\""));
        let outbox = record("host_uri_request", ",\"uri\":\"spice://outbox\"");
        let compacted = record("manual_compaction_end", "");
        let complete = [steering.as_str(), outbox.as_str(), compacted.as_str()].join("\n");
        assert_eq!(capture_gap(&complete, STEER), None);

        // A tool call merely naming the outbox, or text mentioning compaction,
        // is not the native record.
        let mentioned = [
            steering.as_str(),
            &record("tool_execution_start", ",\"path\":\"spice://outbox\""),
            &record(
                "tool_execution_start",
                ",\"i\":\"Reading bounded compaction context\"",
            ),
        ]
        .join("\n");
        assert!(capture_gap(&mentioned, STEER).is_some());

        // Each record counts only after its predecessor.
        let reordered = [compacted.as_str(), outbox.as_str(), steering.as_str()].join("\n");
        assert!(capture_gap(&reordered, STEER).is_some());
        let uncompacted = [steering.as_str(), outbox.as_str()].join("\n");
        assert!(capture_gap(&uncompacted, STEER).is_some());
    }
}
