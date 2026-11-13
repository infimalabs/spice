//! Focused and partial gate command surfaces.

use std::io::Write;

use spice::ambient::Ambient;

use crate::GateArmError;

/// Run one canonical gate arm and emit only its bounded diagnostic on refusal.
///
/// `progress` learns which private snapshot ran when the worktree holds edits
/// that snapshot left out.
///
/// # Errors
///
/// Returns acquisition, output, or non-admitting arm failures.
pub fn gate_arm(
    ambient: &Ambient,
    gate_id: gate::GateId,
    output: &mut impl Write,
    progress: &mut impl Write,
) -> Result<(), GateArmError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (report, snapshot) =
            spice::gate::run_arm_report_strict(ambient, gate::Selector::staged(), gate_id)
                .map_err(GateArmError::Run)?;
        write_snapshot_notice(&snapshot, progress)?;
        if !report.admits() {
            output
                .write_all(report.refusal_fold().as_bytes())
                .map_err(GateArmError::Output)?;
            return Err(GateArmError::Refused(gate_id));
        }
        Ok(())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (ambient, gate_id, output, progress);
        Err(GateArmError::Unavailable)
    }
}

/// Run one canonical gate arm and emit its complete canonical focused report.
///
/// `progress` learns which private snapshot ran when the worktree holds edits
/// that snapshot left out; the report document stays exact.
///
/// # Errors
///
/// Returns acquisition, encoding, output, or non-admitting arm failures.
pub fn gate_arm_report(
    ambient: &Ambient,
    gate_id: gate::GateId,
    output: &mut impl Write,
    progress: &mut impl Write,
) -> Result<(), GateArmError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (report, snapshot) =
            spice::gate::run_arm_report_strict(ambient, gate::Selector::staged(), gate_id)
                .map_err(GateArmError::Run)?;
        write_snapshot_notice(&snapshot, progress)?;
        let document = report.encode().map_err(GateArmError::Encode)?;
        output
            .write_all(&document)
            .and_then(|()| output.write_all(b"\n"))
            .map_err(GateArmError::Output)?;
        if !report.admits() {
            return Err(GateArmError::Refused(gate_id));
        }
        Ok(())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (ambient, gate_id, output, progress);
        Err(GateArmError::Unavailable)
    }
}

/// Say which private snapshot ran instead of the worktree; nothing when the
/// snapshot carries the worktree exactly.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn write_snapshot_notice(
    snapshot: &gate::tree::WorktreeDivergence,
    progress: &mut impl Write,
) -> Result<(), GateArmError> {
    if snapshot.is_empty() {
        return Ok(());
    }
    writeln!(progress, "gate: {snapshot}").map_err(GateArmError::Output)
}
