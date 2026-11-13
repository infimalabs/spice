//! Diagnostic observations made beside one proof execution.
//!
//! These say where an execution's time went and which build work it repeated.
//! They never enter an action key, an input manifest, or a typed result, so two
//! executions that differ only here prove the same thing.

use std::cell::RefCell;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Most trailing output bytes one execution retains on a passing receipt.
pub const OUTPUT_TAIL_LIMIT: usize = 8 * 1024;

/// One measured stage of an execution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionPhase {
    /// The app arm's wasm `cargo clippy`.
    Clippy,
    /// The app arm's `trunk build`: Cargo and `wasm-bindgen`.
    Trunk,
    /// A test shard's workspace-wide `cargo test --no-run`.
    Prebuild,
    /// A Cargo shard's own commands after any prebuild.
    Execution,
}

/// Wall time of one phase.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseTiming {
    pub phase: ExecutionPhase,
    pub millis: u64,
}

/// Cargo `compiler-artifact` messages by freshness.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CargoArtifactCounts {
    /// Units Cargo found up to date.
    pub fresh: u64,
    /// Units Cargo compiled in this execution.
    pub rebuilt: u64,
}

/// Everything diagnostic one execution observed.
///
/// Phase time belongs to the execution that spent it: a prebuild another
/// execution already completed shows here as fresh units and little time, so
/// shared work is never apportioned across receipts.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionDiagnostics {
    /// Time spent waiting for the action's single-flight lease, outside execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_millis: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo: Option<CargoArtifactCounts>,
    /// At most [`OUTPUT_TAIL_LIMIT`] trailing bytes of the trunk build's output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tail: Option<String>,
}

thread_local! {
    static ACTIVE: RefCell<Option<ExecutionDiagnostics>> = const { RefCell::new(None) };
}

/// Run `execution` and return what it recorded on this thread.
///
/// Recording is a no-op outside an observation, and work an execution hands to
/// another thread is not observed.
pub fn observe_execution<T>(execution: impl FnOnce() -> T) -> (T, ExecutionDiagnostics) {
    struct Scope(Option<ExecutionDiagnostics>);
    impl Drop for Scope {
        fn drop(&mut self) {
            ACTIVE.set(self.0.take());
        }
    }

    let scope = Scope(ACTIVE.replace(Some(ExecutionDiagnostics::default())));
    let value = execution();
    let observed = ACTIVE.take().unwrap_or_default();
    drop(scope);
    (value, observed)
}

fn record(update: impl FnOnce(&mut ExecutionDiagnostics)) {
    ACTIVE.with_borrow_mut(|active| {
        if let Some(diagnostics) = active {
            update(diagnostics);
        }
    });
}

pub(crate) fn record_phase(phase: ExecutionPhase, elapsed: Duration) {
    record(|diagnostics| {
        diagnostics.phases.push(PhaseTiming {
            phase,
            millis: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        });
    });
}

/// Count the `compiler-artifact` lines of Cargo JSON output.
pub(crate) fn record_cargo_messages(stdout: &[u8]) {
    record(|diagnostics| {
        let counts = diagnostics.cargo.get_or_insert_default();
        let (fresh, rebuilt) = cargo_artifact_freshness(stdout);
        counts.fresh = counts.fresh.saturating_add(fresh);
        counts.rebuilt = counts.rebuilt.saturating_add(rebuilt);
    });
}

pub(crate) fn record_output_tail(output: &str) {
    record(|diagnostics| diagnostics.output_tail = Some(output_tail(output).to_owned()));
}

#[derive(Deserialize)]
struct ArtifactMessage {
    reason: String,
    #[serde(default)]
    fresh: Option<bool>,
}

fn cargo_artifact_freshness(stdout: &[u8]) -> (u64, u64) {
    let mut fresh = 0_u64;
    let mut rebuilt = 0_u64;
    for line in stdout.split(|byte| *byte == b'\n') {
        if !line.starts_with(b"{") {
            continue;
        }
        let Ok(message) = serde_json::from_slice::<ArtifactMessage>(line) else {
            continue;
        };
        match (message.reason.as_str(), message.fresh) {
            ("compiler-artifact", Some(true)) => fresh += 1,
            ("compiler-artifact", Some(false)) => rebuilt += 1,
            _ => {}
        }
    }
    (fresh, rebuilt)
}

/// The trailing [`OUTPUT_TAIL_LIMIT`] bytes of `output`, on a character boundary.
pub(crate) fn output_tail(output: &str) -> &str {
    let mut start = output.len().saturating_sub(OUTPUT_TAIL_LIMIT);
    while !output.is_char_boundary(start) {
        start += 1;
    }
    &output[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_collects_only_its_own_execution() {
        record_phase(ExecutionPhase::Clippy, Duration::from_millis(5));
        let ((), observed) = observe_execution(|| {
            record_phase(ExecutionPhase::Prebuild, Duration::from_millis(7));
            record_cargo_messages(
                concat!(
                    "{\"reason\":\"compiler-artifact\",\"fresh\":true}\n",
                    "{\"reason\":\"compiler-artifact\",\"fresh\":false}\n",
                    "{\"reason\":\"compiler-message\",\"fresh\":false}\n",
                    "not json\n",
                    "{\"reason\":\"compiler-artifact\",\"fresh\":true}",
                )
                .as_bytes(),
            );
            record_phase(ExecutionPhase::Execution, Duration::from_millis(2));
        });
        assert_eq!(
            observed.phases,
            [
                PhaseTiming {
                    phase: ExecutionPhase::Prebuild,
                    millis: 7,
                },
                PhaseTiming {
                    phase: ExecutionPhase::Execution,
                    millis: 2,
                },
            ]
        );
        assert_eq!(
            observed.cargo,
            Some(CargoArtifactCounts {
                fresh: 2,
                rebuilt: 1,
            })
        );
        let ((), after) = observe_execution(|| {});
        assert_eq!(after, ExecutionDiagnostics::default());
    }

    #[test]
    fn output_tail_keeps_the_end_on_a_character_boundary() {
        let output = format!("{}{}", "é".repeat(OUTPUT_TAIL_LIMIT), "end");
        let tail = output_tail(&output);
        assert!(tail.len() <= OUTPUT_TAIL_LIMIT);
        assert!(tail.ends_with("end"));
        assert_eq!(output_tail("short"), "short");
    }
}
