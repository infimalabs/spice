//! Packaged and configured worker dispatch over one staged source.

use super::*;

pub(super) fn dispatch(
    progress: &mut File,
    started: Instant,
    claim: gate::ClaimCapture,
    profile: &mut profile::Profile,
    ambient: &Ambient,
) -> Result<Report, WorkerFailure> {
    write_phase(progress, &WorkerPhase::Policy).map_err(worker_io_failure)?;
    let dispatch = configured::Dispatch::capture_with_claim(claim.clone(), ambient)
        .map_err(configured_failure)?;
    dispatch_captured(Some(progress), started, claim, dispatch, None, profile)
}

pub(super) fn dispatch_captured(
    mut progress: Option<&mut File>,
    started: Instant,
    claim: gate::ClaimCapture,
    dispatch: configured::Dispatch<'_>,
    audit: Option<&mut ApplianceGenerationAudit>,
    profile: &mut profile::Profile,
) -> Result<Report, WorkerFailure> {
    require_non_planning_claim(&claim)?;
    let source = dispatch.source().clone();
    let (packaged, admission, mut dispatch) = if dispatch.admission_enabled() {
        dispatch
            .require_admission_approved()
            .map_err(configured_failure)?;
        let (handoff, anchor) = dispatch.into_admission().map_err(configured_failure)?;
        let (admission, staged) = admission::run(
            claim,
            handoff.repository(),
            handoff.ambient(),
            anchor,
            audit,
            profile,
        )?
        .into_parts();
        if admission.source() != &source {
            return Err(WorkerFailure {
                reason: "gate::pre-commit-staged-source-moved".to_owned(),
                detail: format!(
                    "dispatcher source {source} differs from admission source {}",
                    admission.source(),
                ),
                fault: false,
            });
        }
        let dispatch = handoff.resume(staged).map_err(configured_failure)?;
        (None, Some(admission), dispatch)
    } else {
        let progress = progress.as_deref_mut().ok_or_else(|| WorkerFailure {
            reason: "gate::pre-commit-progress-unavailable".to_owned(),
            detail: "packaged execution requires a progress stream".to_owned(),
            fault: true,
        })?;
        let (packaged, dispatch) = packaged(progress, dispatch, &source, claim)?;
        (packaged, None, dispatch)
    };

    let configured = if dispatch.has_configured() {
        let progress = progress.ok_or_else(|| WorkerFailure {
            reason: "gate::pre-commit-progress-unavailable".to_owned(),
            detail: "configured execution requires a progress stream".to_owned(),
            fault: true,
        })?;
        write_phase(
            progress,
            &WorkerPhase::Configured {
                steps: dispatch.configured_count(),
            },
        )
        .map_err(worker_io_failure)?;
        let remaining = WORK_RUNTIME
            .saturating_sub(started.elapsed())
            .saturating_sub(configured::cleanup_reserve());
        let configured = dispatch
            .execute_configured(remaining, Cancellation::Never)
            .map_err(configured_failure)?;
        dispatch.verify().map_err(configured_failure)?;
        configured
    } else {
        None
    };
    Report::new(source, packaged, admission, configured).map_err(|detail| WorkerFailure {
        reason: "gate::pre-commit-report-invalid".to_owned(),
        detail,
        fault: true,
    })
}

pub(super) fn require_non_planning_claim(claim: &gate::ClaimCapture) -> Result<(), WorkerFailure> {
    if let gate::ClaimCapture::Active { task, phase } = claim
        && gate::lacks_commit_authority(phase.as_str())
    {
        return Err(WorkerFailure {
            reason: "gate::pre-commit-planning-implementation".to_owned(),
            detail: format!(
                "git commit refused: active claimed task {task} is in {} phase",
                phase.as_str()
            ),
            fault: false,
        });
    }
    Ok(())
}

type Builtins<'a> = (
    Option<spice::change_gate::ChangeGateReport>,
    configured::Dispatch<'a>,
);

fn packaged<'a>(
    progress: &mut File,
    mut dispatch: configured::Dispatch<'a>,
    source: &Digest,
    claim: gate::ClaimCapture,
) -> Result<Builtins<'a>, WorkerFailure> {
    if !dispatch.builtins_enabled() {
        return Ok((None, dispatch));
    }
    write_phase(progress, &WorkerPhase::TestSelection).map_err(worker_io_failure)?;
    let report = spice::change_gate::run_with_claim(progress, dispatch.ambient(), claim)
        .map_err(change_gate_failure)?;
    if report.selection().source() != source {
        return Err(WorkerFailure {
            reason: "gate::pre-commit-staged-source-moved".to_owned(),
            detail: format!(
                "dispatcher source {source} differs from packaged source {}",
                report.selection().source(),
            ),
            fault: false,
        });
    }
    dispatch = dispatch.rebind(source).map_err(configured_failure)?;
    Ok((Some(report), dispatch))
}
