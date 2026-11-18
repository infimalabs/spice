use super::record::{
    InstallRecord, LoadedRecord, RefreshRecord, read_record_for_refresh, read_record_for_unapply,
    record_path, remove_record, write_initial_record, write_record,
};
use super::*;

/// An install plan derived and admitted in this process, carried into apply.
///
/// It holds the receipt image it was derived from as a compare-and-swap
/// witness instead of deriving the plan again. Before the first mutation,
/// apply still proves every source byte it publishes, every destination
/// binding and image, and that the receipt is the one this plan superseded.
pub struct PreparedInstall {
    record: InstallRecord,
    receipt: Option<ReceiptGeneration>,
    fresh: bool,
}

impl PreparedInstall {
    /// Exact plan apply will execute.
    #[must_use]
    pub const fn plan(&self) -> &InstallPlan {
        &self.record.plan
    }

    /// Canonical digest of [`Self::plan`].
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.record.plan_digest
    }

    pub(super) fn into_plan(self) -> InstallPlan {
        self.record.plan
    }

    /// Whether apply would find this plan already complete and publish
    /// nothing: a complete install of this same build and hook topology.
    #[must_use]
    pub fn converged(&self) -> bool {
        !self.fresh && self.record.status == InstallStatus::Complete
    }
}

pub(super) fn apply_expected(
    repository: &Repository,
    spice_binary: Option<&Path>,
    expected: &Digest,
    lifecycle_owner: Option<LifecycleOwnerStanding>,
) -> Result<InstallReceipt, InstallError> {
    let prepared = prepare(repository, spice_binary, lifecycle_owner)?;
    if prepared.digest() != expected {
        return Err(InstallError::DigestMismatch {
            expected: expected.clone(),
            observed: prepared.record.plan_digest,
        });
    }
    apply_prepared_record(repository, spice_binary, prepared)
}

pub(super) fn apply_prepared_record(
    repository: &Repository,
    spice_binary: Option<&Path>,
    prepared: PreparedInstall,
) -> Result<InstallReceipt, InstallError> {
    let PreparedInstall {
        mut record,
        receipt: record_generation,
        fresh,
    } = prepared;
    reclaim_temporary_records(repository)?;
    match record.status {
        InstallStatus::Complete | InstallStatus::Establishing => return Ok(record.receipt()),
        InstallStatus::Unapplying => return Err(InstallError::UnapplyInProgress),
        InstallStatus::Applying | InstallStatus::Uninstalled => {}
    }
    let symbols = spice_binary
        .map(|binary| symbols::Source::resolve(repository, binary))
        .transpose()?
        .flatten();
    let generated = materialize_operations(&record, spice_binary, symbols.as_ref())?;
    if fresh {
        record.plan.validate_pristine(repository)?;
    } else {
        record.plan.validate_for_repository(repository)?;
    }
    require_receipt(repository, &record.plan, record_generation.as_ref())?;
    let mut record_generation = match record_generation {
        Some(generation) => write_record(repository, &record, generation)?,
        None => write_initial_record(repository, &record)?,
    };
    // The applying receipt owns fixed-size reversal references before any
    // destination changes. Persist every referenced payload once, then keep
    // progress checkpoints independent of those payloads' byte lengths.
    reversal::ensure_payloads(repository, &record.plan)?;
    record_generation =
        apply_file_operations(repository, &mut record, &generated, record_generation)?;
    record_generation = apply_retirements(repository, &mut record, record_generation)?;
    // A plan naming a lifecycle owner settles only once the command layer has
    // registered and started that owner under the installed executable.
    record.status = if record.plan.lifecycle_owner.is_some() {
        InstallStatus::Establishing
    } else {
        InstallStatus::Complete
    };
    write_record(repository, &record, record_generation)?;
    Ok(record.receipt())
}

/// Settle an install whose files are published and whose lifecycle owner now
/// serves under the installed executable.
///
/// A complete record answers its receipt unchanged; an in-flight direction or
/// an absent install is refused.
pub(super) fn complete_lifecycle(repository: &Repository) -> Result<InstallReceipt, InstallError> {
    let loaded = read_record(repository)?.ok_or(InstallError::RecordMissing)?;
    let mut record = loaded.record;
    record.validate_for_repository(repository)?;
    match record.status {
        InstallStatus::Establishing => {}
        InstallStatus::Complete => return Ok(record.receipt()),
        InstallStatus::Applying => return Err(InstallError::ApplyInProgress),
        InstallStatus::Unapplying => return Err(InstallError::UnapplyInProgress),
        InstallStatus::Uninstalled => return Err(InstallError::RecordMissing),
    }
    verify_progress(repository, &record)?;
    record.status = InstallStatus::Complete;
    write_record(repository, &record, loaded.generation)?;
    Ok(record.receipt())
}

/// The receipt a plan was derived from must still be the one it supersedes.
///
/// Comparing the native generation and complete image proves this without
/// decoding the receipt again; every later receipt write repeats the same
/// compare-and-swap.
fn require_receipt(
    repository: &Repository,
    plan: &InstallPlan,
    expected: Option<&ReceiptGeneration>,
) -> Result<(), InstallError> {
    let (root, _) = record_location(repository, &plan.record)?;
    let (observed, _) = observe_bound(&root, &plan.record.destination, &plan.record.parent)?;
    let unchanged = match (expected, observed.as_ref()) {
        (None, None) => true,
        (Some(expected), Some(observed)) => expected.matches(observed),
        _ => false,
    };
    if unchanged {
        Ok(())
    } else {
        Err(InstallError::ParentChanged(record_path(repository)?))
    }
}

fn apply_file_operations(
    repository: &Repository,
    record: &mut InstallRecord,
    generated: &[FileImage],
    mut record_generation: ReceiptGeneration,
) -> Result<ReceiptGeneration, InstallError> {
    let mut durable_completed = record.completed;
    while record.completed < record.plan.operations.len() {
        let index = record.completed;
        let (generation, changed) = apply_operation(
            repository,
            record,
            &generated[index],
            index,
            record_generation,
            durable_completed,
        )?;
        record_generation = generation;
        record.completed += 1;
        if changed {
            record_generation = write_record(repository, record, record_generation)?;
            durable_completed = record.completed;
        }
    }
    if durable_completed != record.completed {
        record_generation = write_record(repository, record, record_generation)?;
    }
    Ok(record_generation)
}

fn apply_retirements(
    repository: &Repository,
    record: &mut InstallRecord,
    mut record_generation: ReceiptGeneration,
) -> Result<ReceiptGeneration, InstallError> {
    while record.completed < record.plan.operations.len() + record.plan.retirements.len() {
        let index = record.completed - record.plan.operations.len();
        if record.plan.retirements[index].discards_payload() {
            record_generation = apply_discards(repository, record, index, record_generation)?;
            continue;
        }
        record.plan.retirements[index].validate_binding(repository)?;
        let parent = retire_path(repository, &record.plan.retirements[index], index)?;
        record.retired_parents[index] = parent;
        record.completed += 1;
        record_generation = write_record(repository, record, record_generation)?;
    }
    Ok(record_generation)
}

/// Retire one run of discards sharing a parent binding as one durable step.
///
/// Every temporary name in the run is proved free and fenced in the receipt
/// before any file moves, so resume finishes each file from wherever it
/// stopped. The run then withdraws the parent its binding names as missing
/// and advances progress past all of it with one more receipt write, rather
/// than two writes per file.
fn apply_discards(
    repository: &Repository,
    record: &mut InstallRecord,
    start: usize,
    mut record_generation: ReceiptGeneration,
) -> Result<ReceiptGeneration, InstallError> {
    let retirements = &record.plan.retirements;
    let end = start
        + retirements[start..]
            .iter()
            .take_while(|retirement| {
                retirement.discards_payload() && retirement.parent == retirements[start].parent
            })
            .count();
    for retirement in &retirements[start..end] {
        retirement.validate_binding(repository)?;
    }
    if record.retired_parents[start..end]
        .iter()
        .any(Option::is_none)
    {
        for index in start..end {
            if record.retired_parents[index].is_none() {
                record.retired_parents[index] =
                    prepare_retirement(repository, &record.plan.retirements[index], index)?;
            }
        }
        record_generation = write_record(repository, record, record_generation)?;
    }
    let operations = record.plan.operations.len();
    for index in start..end {
        record.retired_parents[index] =
            retire_path(repository, &record.plan.retirements[index], index)?;
    }
    let last = &record.plan.retirements[end - 1];
    if last.withdraws_parent(record.plan.retirements.get(end)) {
        withdraw_parent(repository, last)?;
    }
    record.completed = operations + end;
    write_record(repository, record, record_generation)
}

fn materialize_operations(
    record: &InstallRecord,
    spice_binary: Option<&Path>,
    symbols: Option<&symbols::Source>,
) -> Result<Vec<FileImage>, InstallError> {
    record
        .plan
        .operations
        .iter()
        .map(|operation| {
            materialize_generated(
                operation,
                spice_binary,
                symbols,
                &record.plan.repository.common_git,
            )
        })
        .collect()
}

/// Derive the one plan apply would execute now, without mutating anything.
pub(super) fn prepare(
    repository: &Repository,
    spice_binary: Option<&Path>,
    lifecycle_owner: Option<LifecycleOwnerStanding>,
) -> Result<PreparedInstall, InstallError> {
    super::ref_storage::verify_files_storage(repository)?;
    match read_record_for_refresh(repository)? {
        Some(refresh) => prepare_over(repository, spice_binary, refresh, lifecycle_owner),
        None => fresh(build_plan(repository, spice_binary, lifecycle_owner)?, None),
    }
}

/// Derive the one plan apply would execute over an already loaded receipt.
pub(super) fn prepare_over(
    repository: &Repository,
    spice_binary: Option<&Path>,
    refresh: RefreshRecord,
    lifecycle_owner: Option<LifecycleOwnerStanding>,
) -> Result<PreparedInstall, InstallError> {
    let repair = refresh.is_repairable();
    let LoadedRecord { record, generation } = refresh.into_loaded();
    if record.status == InstallStatus::Uninstalled {
        verify_progress(repository, &record)?;
        return fresh(
            build_plan(repository, spice_binary, lifecycle_owner)?,
            Some(generation),
        );
    }
    if record.status == InstallStatus::Complete
        && (repair
            || refresh_required(
                repository,
                &record.plan,
                spice_binary,
                lifecycle_owner.as_ref(),
            )?)
    {
        return fresh(
            refresh_plan(repository, spice_binary, &record, lifecycle_owner)?,
            Some(generation),
        );
    }
    verify_binary_context(&record.plan, spice_binary)?;
    verify_progress(repository, &record)?;
    Ok(PreparedInstall {
        record,
        receipt: Some(generation),
        fresh: false,
    })
}

/// A plan this process derived from nothing, or over `receipt`, ready to apply.
pub(super) fn fresh(
    plan: InstallPlan,
    receipt: Option<ReceiptGeneration>,
) -> Result<PreparedInstall, InstallError> {
    Ok(PreparedInstall {
        record: InstallRecord::new(plan)?,
        receipt,
        fresh: true,
    })
}

fn refresh_plan(
    repository: &Repository,
    spice_binary: Option<&Path>,
    record: &InstallRecord,
    lifecycle_owner: Option<LifecycleOwnerStanding>,
) -> Result<InstallPlan, InstallError> {
    // One record swap derives a complete current plan before any destination
    // changes. Binary-only drift is the sole reconciled input among the
    // assets this build publishes; fixed assets remain receipt-strict. A
    // recorded operation this build no longer publishes is retiring: the plan
    // decides between discarding it and orphaning it from what it observes.
    let symbols = spice_binary
        .map(|binary| symbols::Source::resolve(repository, binary))
        .transpose()?
        .flatten();
    let published = published_paths(spice_binary.is_some(), symbols.is_some())?;
    let binary_superseded = verify_refresh_progress(repository, record, |operation| {
        !published.contains(&operation.path)
    })?;
    build_plan_over(
        repository,
        spice_binary,
        Some(&record.plan),
        binary_superseded.as_ref(),
        symbols,
        lifecycle_owner,
    )
}

fn refresh_required(
    repository: &Repository,
    plan: &InstallPlan,
    spice_binary: Option<&Path>,
    lifecycle_owner: Option<&LifecycleOwnerStanding>,
) -> Result<bool, InstallError> {
    Ok(current_build_differs(repository, plan, spice_binary)?
        || plan.lifecycle_owner() != lifecycle_owner)
}
fn apply_operation(
    repository: &Repository,
    record: &mut InstallRecord,
    generated: &FileImage,
    index: usize,
    mut record_generation: ReceiptGeneration,
    durable_completed: usize,
) -> Result<(ReceiptGeneration, bool), InstallError> {
    let operation = &record.plan.operations[index];
    operation.validate_binding(repository)?;
    let (current, parent) = observe_bound(
        operation.location.root(repository),
        &operation.destination,
        &operation.parent,
    )?;
    let changed = !matches_generated(current.as_ref(), operation);
    let (generation, parent) = if !changed {
        (
            validate_existing_generation(
                repository,
                record,
                operation,
                current.as_ref(),
                index,
                parent,
            )?,
            parent.ok_or_else(|| InstallError::ParentChanged(operation.destination(repository)))?,
        )
    } else if operation.expects_in_place(current.as_ref()) {
        // Unchanged operations need no checkpoint. Before the next mutation,
        // persist their progress so a crash can leave only the recorded
        // frontier's file ahead of its receipt, exactly as resume expects.
        if durable_completed != index {
            record_generation = write_record(repository, record, record_generation)?;
        }
        publish_operation(repository, operation, generated)?
    } else {
        return Err(drift(operation, current));
    };
    record.installed[index] = Some(generation);
    record.installed_parents[index] = Some(parent);
    Ok((record_generation, changed))
}

fn validate_existing_generation(
    repository: &Repository,
    record: &InstallRecord,
    operation: &Operation,
    current: Option<&ObservedFile>,
    index: usize,
    parent: Option<Generation>,
) -> Result<Generation, InstallError> {
    let generation = current.expect("a generated match is present").generation;
    if record.installed_parents[index].is_some_and(|expected| Some(expected) != parent) {
        return Err(InstallError::ParentChanged(
            operation.destination(repository),
        ));
    }
    Ok(generation)
}

pub(super) fn unapply_record(repository: &Repository) -> Result<InstallReceipt, InstallError> {
    let (loaded, requires_publish) =
        read_record_for_unapply(repository)?.ok_or(InstallError::RecordMissing)?;
    let mut record = loaded.record;
    let mut record_generation = loaded.generation;
    // Reversal is an executable direction. Supported retained schemas cross
    // into this build's current model before the canonical reversal begins.
    record.validate_for_repository(repository)?;
    match record.status {
        InstallStatus::Applying => return Err(InstallError::ApplyInProgress),
        InstallStatus::Uninstalled => {
            verify_progress(repository, &record)?;
            if requires_publish {
                write_record(repository, &record, record_generation)?;
            }
            return Ok(record.receipt());
        }
        InstallStatus::Establishing | InstallStatus::Complete => {
            verify_progress(repository, &record)?;
            reversal::ensure_payloads(repository, &record.plan)?;
            record.status = InstallStatus::Unapplying;
            record.completed = 0;
            record.restored.fill(None);
            record.restored_retirements.fill(None);
            record_generation = write_record(repository, &record, record_generation)?;
        }
        InstallStatus::Unapplying => {
            verify_progress(repository, &record)?;
            reversal::ensure_payloads(repository, &record.plan)?;
        }
    }

    record_generation = reverse_operations(repository, &mut record, record_generation)?;
    record.status = InstallStatus::Uninstalled;
    let receipt = record.receipt();
    write_record(repository, &record, record_generation)?;
    Ok(receipt)
}

pub(super) fn retire_uninstalled_record(repository: &Repository) -> Result<(), InstallError> {
    let LoadedRecord { record, generation } =
        read_record(repository)?.ok_or(InstallError::RecordMissing)?;
    record.validate_for_repository(repository)?;
    if record.status != InstallStatus::Uninstalled {
        return Err(InstallError::InvalidRecord(
            "install receipt is not ready for retirement".to_owned(),
        ));
    }
    verify_progress(repository, &record)?;
    reversal::remove_payloads(repository, &record.plan)?;
    remove_record(repository, &record, generation)
}

fn reverse_operations(
    repository: &Repository,
    record: &mut InstallRecord,
    mut record_generation: ReceiptGeneration,
) -> Result<ReceiptGeneration, InstallError> {
    while record.completed < record.plan.operation_count() {
        if let Some(generation) = reverse_retirement(repository, record, record_generation.clone())?
        {
            record_generation = generation;
            continue;
        }
        record_generation = reverse_file_operation(repository, record, record_generation)?;
    }
    Ok(record_generation)
}

fn reverse_retirement(
    repository: &Repository,
    record: &mut InstallRecord,
    mut record_generation: ReceiptGeneration,
) -> Result<Option<ReceiptGeneration>, InstallError> {
    let completed = record.completed;
    if completed >= record.plan.retirements.len() {
        return Ok(None);
    }
    let index = record.plan.retirements.len() - 1 - completed;
    let retirement = &record.plan.retirements[index];
    retirement.validate_binding(repository)?;
    let (current, parent) = observe_bound(
        repository.common_dir(),
        &retirement.destination,
        &retirement.parent,
    )?;
    let already_restored = images_match(current.as_ref(), retirement.previous.as_ref());
    let installed = current.is_none() && parent == record.retired_parents[index];
    if !already_restored && !installed {
        return Err(retirement_drift(retirement, current));
    }
    if !already_restored {
        let previous = retirement
            .previous
            .as_ref()
            .map(|reference| reversal::load_payload(repository, reference))
            .transpose()?;
        record.restored_retirements[index] =
            restore_retirement(repository, retirement, previous.as_ref())?;
    } else {
        record.restored_retirements[index] = current.map(|observed| observed.generation);
    }
    record.completed += 1;
    record_generation = write_record(repository, record, record_generation)?;
    Ok(Some(record_generation))
}

fn reverse_file_operation(
    repository: &Repository,
    record: &mut InstallRecord,
    record_generation: ReceiptGeneration,
) -> Result<ReceiptGeneration, InstallError> {
    let non_file_offset = record.plan.retirements.len();
    let file_completed = record.completed - non_file_offset;
    let reverse = record.plan.operations.len() - 1 - file_completed;
    let operation = &record.plan.operations[reverse];
    operation.validate_binding(repository)?;
    let (current, parent) = observe_bound(
        operation.location.root(repository),
        &operation.destination,
        &operation.parent,
    )?;
    let (restored, needs_cleanup) = restored_image(
        repository,
        record,
        operation,
        operation.previous.as_ref(),
        reverse,
        current.as_ref(),
        parent,
    )?;
    record.restored[reverse] = restored;
    if needs_cleanup {
        cleanup_operation(repository, operation)?;
    }
    record.completed += 1;
    write_record(repository, record, record_generation)
}

fn restored_image(
    repository: &Repository,
    record: &InstallRecord,
    operation: &Operation,
    previous: Option<&GeneratedImage>,
    reverse: usize,
    current: Option<&ObservedFile>,
    parent: Option<Generation>,
) -> Result<(Option<Generation>, bool), InstallError> {
    if restored_matches(current, previous, record.restored[reverse]) {
        return Ok((record.restored[reverse], true));
    }
    if images_match(current, previous) {
        return Ok((current.map(|observed| observed.generation), true));
    }
    if installed_matches(
        current,
        operation,
        record.installed[reverse],
        parent,
        record.installed_parents[reverse],
    ) {
        let generation = current
            .expect("an installed image match is present")
            .generation;
        let previous = previous
            .map(|reference| reversal::load_payload(repository, reference))
            .transpose()?;
        return Ok((
            restore_operation(repository, operation, generation, previous.as_ref())?,
            false,
        ));
    }
    Err(drift(operation, current.cloned()))
}

pub(super) fn verify_progress(
    repository: &Repository,
    record: &InstallRecord,
) -> Result<(), InstallError> {
    record.validate_loaded_for_repository(repository)?;
    verify_file_progress(repository, record, reversed_file_count(record))?;
    verify_retirement_progress(repository, record)?;
    Ok(())
}

fn reversed_file_count(record: &InstallRecord) -> usize {
    match record.status {
        InstallStatus::Unapplying => record
            .completed
            .saturating_sub(record.plan.retirements.len()),
        _ => record.completed.min(record.plan.operations.len()),
    }
}

fn verify_retirement_progress(
    repository: &Repository,
    record: &InstallRecord,
) -> Result<(), InstallError> {
    let applying_completed = record
        .completed
        .saturating_sub(record.plan.operations.len())
        .min(record.plan.retirements.len());
    let reversed = record.completed.min(record.plan.retirements.len());
    for (index, retirement) in record.plan.retirements.iter().enumerate() {
        let previous = retirement.previous.as_ref();
        let (observed, parent) = if record.settled() && retirement.discards_payload() {
            // A finished discard's recorded parent may since have been
            // removed; its file must stay absent wherever the path binds now.
            let bound = bind_destination(repository.common_dir(), &retirement.path)?;
            observe_bound(repository.common_dir(), &bound.destination, &bound.parent)?
        } else {
            observe_bound(
                repository.common_dir(),
                &retirement.destination,
                &retirement.parent,
            )?
        };
        // A discard restores nothing, so its parent carries no reversal
        // authority: a run that withdrew its emptied directory is still absent.
        let absent = observed.is_none()
            && (retirement.discards_payload() || parent == record.retired_parents[index]);
        let prior = images_match(observed.as_ref(), previous);
        let planned = retirement.expects_in_place(observed.as_ref());
        // A fenced discard run may be interrupted after any of its files.
        let in_flight = index == applying_completed
            || (retirement.discards_payload() && record.retired_parents[index].is_some());
        let valid = match record.status {
            InstallStatus::Applying if index < applying_completed => absent,
            InstallStatus::Applying if in_flight => planned || absent,
            InstallStatus::Applying => planned,
            InstallStatus::Establishing | InstallStatus::Complete => observed.is_none(),
            InstallStatus::Unapplying => {
                let reverse = record.plan.retirements.len() - 1 - index;
                if reverse < reversed {
                    prior
                } else if reverse == reversed {
                    absent || prior
                } else {
                    absent
                }
            }
            InstallStatus::Uninstalled => prior,
        };
        if !valid {
            return Err(retirement_drift(retirement, observed));
        }
    }
    Ok(())
}

fn verify_file_progress(
    repository: &Repository,
    record: &InstallRecord,
    file_completed: usize,
) -> Result<(), InstallError> {
    for (index, operation) in record.plan.operations.iter().enumerate() {
        let (observed, parent) = observe_bound(
            operation.location.root(repository),
            &operation.destination,
            &operation.parent,
        )?;
        let valid = file_progress_valid(
            record,
            operation,
            operation.previous.as_ref(),
            index,
            observed.as_ref(),
            parent,
            file_completed,
        );
        if !valid {
            return Err(drift(operation, observed));
        }
    }
    Ok(())
}

fn file_progress_valid(
    record: &InstallRecord,
    operation: &Operation,
    previous: Option<&GeneratedImage>,
    index: usize,
    observed: Option<&ObservedFile>,
    parent: Option<Generation>,
    file_completed: usize,
) -> bool {
    match record.status {
        InstallStatus::Applying => record.installed[index].map_or_else(
            || {
                operation.expects_in_place(observed)
                    || (index == record.completed && matches_generated(observed, operation))
            },
            |generation| {
                installed_matches(
                    observed,
                    operation,
                    Some(generation),
                    parent,
                    record.installed_parents[index],
                )
            },
        ),
        InstallStatus::Establishing | InstallStatus::Complete => installed_matches(
            observed,
            operation,
            record.installed[index],
            parent,
            record.installed_parents[index],
        ),
        InstallStatus::Unapplying => unapplying_file_valid(
            record,
            operation,
            previous,
            index,
            observed,
            parent,
            file_completed,
        ),
        InstallStatus::Uninstalled => restored_matches(observed, previous, record.restored[index]),
    }
}

fn unapplying_file_valid(
    record: &InstallRecord,
    operation: &Operation,
    previous: Option<&GeneratedImage>,
    index: usize,
    observed: Option<&ObservedFile>,
    parent: Option<Generation>,
    file_completed: usize,
) -> bool {
    let reversed = record.plan.operations.len() - 1 - index;
    if reversed < file_completed {
        return restored_matches(observed, previous, record.restored[index]);
    }
    if reversed == file_completed {
        return restored_matches(observed, previous, record.restored[index])
            || images_match(observed, previous)
            || installed_matches(
                observed,
                operation,
                record.installed[index],
                parent,
                record.installed_parents[index],
            );
    }
    installed_matches(
        observed,
        operation,
        record.installed[index],
        parent,
        record.installed_parents[index],
    )
}

/// Verify a completed install before deriving a rebuilt-binary refresh.
///
/// Every receipt-owned destination remains strict except the executable and
/// the operations `retiring` selects. A regular executable under the same
/// recorded parent may carry different bytes or mode: the returned image
/// becomes the exact superseded side of the refresh plan, so its digest admits
/// that reconciliation and apply still compare-and-swaps against what preview
/// observed. A retiring destination is left to the plan, which removes it only
/// while it holds the recorded image and otherwise orphans it.
pub(super) fn verify_refresh_progress(
    repository: &Repository,
    record: &InstallRecord,
    retiring: impl Fn(&Operation) -> bool,
) -> Result<Option<GeneratedImage>, InstallError> {
    record.validate_loaded_for_repository(repository)?;
    if record.status != InstallStatus::Complete {
        verify_progress(repository, record)?;
        return Ok(None);
    }

    let mut binary_superseded = None;
    for (index, operation) in record.plan.operations.iter().enumerate() {
        if retiring(operation) {
            continue;
        }
        let (observed, parent) = observe_bound(
            operation.location.root(repository),
            &operation.destination,
            &operation.parent,
        )?;
        if installed_matches(
            observed.as_ref(),
            operation,
            record.installed[index],
            parent,
            record.installed_parents[index],
        ) {
            continue;
        }
        if operation.asset == Asset::Binary
            && parent == record.installed_parents[index]
            && parent.is_some()
            && let Some(observed) = observed.as_ref()
        {
            binary_superseded = Some(observed.generated());
            continue;
        }
        return Err(drift(operation, observed));
    }
    verify_retirement_progress(repository, record)?;
    Ok(binary_superseded)
}

fn installed_matches(
    observed: Option<&ObservedFile>,
    operation: &Operation,
    generation: Option<Generation>,
    observed_parent: Option<Generation>,
    expected_parent: Option<Generation>,
) -> bool {
    matches!((observed, generation), (Some(observed), Some(_))
        if observed_parent == expected_parent
            && expected_parent.is_some()
            && matches_generated(Some(observed), operation))
}

fn restored_matches(
    observed: Option<&ObservedFile>,
    previous: Option<&GeneratedImage>,
    generation: Option<Generation>,
) -> bool {
    match (previous, observed, generation) {
        (None, None, None) => true,
        (Some(previous), Some(observed), Some(_)) => observed.generated() == *previous,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
