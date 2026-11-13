//! Store faults the resident answers by waiting for the next sweep.

use std::io::Write;

use wire::{FaultClass, Outcome};

use super::CommandResult;

/// Answer a store fault from durable housekeeping by deferring it, not dying.
///
/// The housekeeping arms -- accounting an exit, attempting a lifecycle wake --
/// derive everything they commit from durable inputs the next sweep reads
/// again: a supervisor's state file, a held claim, a pending inbox. A commit
/// they could not land leaves those inputs exactly as they were, so the
/// truthful response is to say so and let the next sweep re-derive, not to
/// end the resident and strand every lane behind the one write that lost a
/// lock. Nothing is retried here; the sweep that follows judges the same
/// durable state afresh. Any other outcome passes through untouched: a
/// protocol or environment fault names a defect no later sweep can read
/// away.
pub(super) fn defer_store_fault(
    result: CommandResult<()>,
    diagnostic: &mut dyn Write,
    arm: &str,
) -> CommandResult<()> {
    match result {
        Err(Outcome::Fault(fault)) if fault.class == FaultClass::Store => {
            let _ = writeln!(
                diagnostic,
                "{arm} deferred to the next reconciliation: {}: {}",
                fault.reason, fault.detail
            );
            Ok(())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use wire::Fault;

    use super::*;

    fn fault(class: FaultClass) -> CommandResult<()> {
        Err(Outcome::Fault(Fault {
            class,
            reason: "journal::commit-failed".to_owned(),
            detail: "database is locked".to_owned(),
        }))
    }

    #[test]
    fn only_a_store_fault_is_deferred_and_it_is_written_down() {
        let mut diagnostic = Vec::new();
        defer_store_fault(fault(FaultClass::Store), &mut diagnostic, "exit accounting")
            .expect("a store fault is deferred");
        assert_eq!(
            String::from_utf8(diagnostic).expect("diagnostic is text"),
            "exit accounting deferred to the next reconciliation: journal::commit-failed: database is locked\n"
        );

        for class in [FaultClass::Protocol, FaultClass::Environment] {
            let mut diagnostic = Vec::new();
            let passed = defer_store_fault(fault(class), &mut diagnostic, "exit accounting")
                .expect_err("a non-store fault passes through");
            assert!(matches!(passed, Outcome::Fault(fault) if fault.class == class));
            assert!(
                diagnostic.is_empty(),
                "a passed-through fault writes nothing"
            );
        }

        let mut diagnostic = Vec::new();
        defer_store_fault(Ok(()), &mut diagnostic, "exit accounting").expect("success passes");
        assert!(diagnostic.is_empty());
    }
}
