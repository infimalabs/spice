use super::*;

pub(super) fn validate_plan(
    plan: &InstallPlan,
    content: GeneratedContentAuthority,
) -> Result<(), InstallError> {
    validate_plan_version(plan)?;
    validate_operation_inventory(plan, content)?;
    validate_plan_destinations(plan, content)
}

fn validate_plan_version(plan: &InstallPlan) -> Result<(), InstallError> {
    if plan.version != PLAN_VERSION {
        return Err(InstallError::InvalidRecord(format!(
            "install plan version {} is not {PLAN_VERSION}",
            plan.version
        )));
    }
    Ok(())
}

/// A plan this build derives matches its canonical inventory and order. A
/// historical receipt is evidence of what another release published, so it is
/// held only to what is intrinsic to it: the assets that must be planned
/// together, and each operation's own binding, validated per destination.
fn validate_operation_inventory(
    plan: &InstallPlan,
    content: GeneratedContentAuthority,
) -> Result<(), InstallError> {
    let historical = content == GeneratedContentAuthority::HistoricalReceipt;
    if !historical {
        validate_canonical_inventory(plan)?;
    }
    validate_planned_together(plan)?;
    Ok(())
}

/// The fixed prefix in order, then the executable and its symbols, which are
/// the only optional operations and close the inventory: `[]`, `[Binary]`, or
/// `[Binary, SymbolsInfo, SymbolsDwarf]`.
fn validate_canonical_inventory(plan: &InstallPlan) -> Result<(), InstallError> {
    let fixed = FIXED_ASSETS.len();
    if plan.operations.len() < fixed
        || plan
            .operations
            .iter()
            .zip(FIXED_ASSETS)
            .any(|(operation, asset)| operation.asset != asset)
    {
        return Err(InstallError::InvalidRecord(
            "install plan operation order is not canonical".to_owned(),
        ));
    }
    let tail = plan.operations[fixed..]
        .iter()
        .map(|operation| operation.asset.known())
        .collect::<Vec<_>>();
    let with_symbols = [Asset::Binary, Asset::SymbolsInfo, Asset::SymbolsDwarf].map(Some);
    if !(tail.is_empty() || tail == [Some(Asset::Binary)] || tail == with_symbols) {
        return Err(InstallError::InvalidRecord(
            "optional executable and symbols do not close the operation inventory".to_owned(),
        ));
    }
    Ok(())
}

/// The symbol provenance pairs with both retained symbol operations; the
/// canvas generation pairs with the executable that links its archive.
fn validate_planned_together(plan: &InstallPlan) -> Result<(), InstallError> {
    let publishes = |asset: Asset| {
        plan.operations
            .iter()
            .any(|operation| operation.asset == asset)
    };
    let symbol_operations = symbols::ASSETS
        .into_iter()
        .filter(|asset| publishes(*asset))
        .count();
    if plan.symbols.is_some() != (symbol_operations == symbols::ASSETS.len())
        || (symbol_operations != 0 && symbol_operations != symbols::ASSETS.len())
    {
        return Err(InstallError::InvalidRecord(
            "symbol provenance and retained symbol operations must be planned together".to_owned(),
        ));
    }
    if let Some(symbols) = &plan.symbols {
        symbols.validate()?;
    }
    // The canvas generation pairs with the executable that links its archive.
    let installed_executable = publishes(Asset::Binary);
    if plan.app.is_some() != installed_executable {
        return Err(InstallError::InvalidRecord(
            "canvas generation and installed executable must be planned together".to_owned(),
        ));
    }
    Ok(())
}

/// Every operation, retirement, and orphan names a distinct safe relative
/// path; a historical receipt's operations are checked only intrinsically,
/// a current plan's also against each asset's contract.
fn validate_plan_destinations(
    plan: &InstallPlan,
    content: GeneratedContentAuthority,
) -> Result<(), InstallError> {
    if !plan.repository.common_git.is_absolute() {
        return Err(InstallError::InvalidRecord(
            "install repository identity is not common-directory scoped".to_owned(),
        ));
    }
    validate_relative(&plan.record.destination)?;
    let mut introduced = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    for operation in &plan.operations {
        validate_operation(
            operation,
            &mut introduced,
            content,
            &plan.repository.common_git,
        )?;
        if !destinations.insert((operation.location, operation.destination.clone())) {
            return Err(InstallError::InvalidRecord(
                "install plan duplicates a physical destination".to_owned(),
            ));
        }
    }
    for retirement in &plan.retirements {
        validate_retirement(retirement, content, &mut introduced)?;
        if !destinations.insert((Location::CommonGit, retirement.destination.clone())) {
            return Err(InstallError::InvalidRecord(
                "install plan duplicates a retirement destination".to_owned(),
            ));
        }
    }
    let mut paths = plan
        .operations
        .iter()
        .map(|operation| operation.path.as_str())
        .chain(
            plan.retirements
                .iter()
                .map(|retirement| retirement.path.as_str()),
        )
        .collect::<BTreeSet<_>>();
    for orphan in &plan.orphans {
        validate_relative(&orphan.path)?;
        if !orphan.recorded.validate()
            || !orphan.observed.validate()
            || orphan.recorded == orphan.observed
        {
            return Err(InstallError::InvalidRecord(
                "install orphan carries an invalid or undrifted file image".to_owned(),
            ));
        }
        if !paths.insert(orphan.path.as_str()) {
            return Err(InstallError::InvalidRecord(
                "install plan orphans a path it also publishes or retires".to_owned(),
            ));
        }
    }
    Ok(())
}

/// A discard restores nothing, so it carries no reversal image and expects no
/// prior image. A plan this build derives retires nothing pointlessly: every
/// retirement it keeps either restores a recorded image or discards a file.
fn validate_retirement(
    retirement: &Retirement,
    content: GeneratedContentAuthority,
    introduced: &mut BTreeSet<(Location, String)>,
) -> Result<(), InstallError> {
    validate_relative(&retirement.path)?;
    validate_relative(&retirement.destination)?;
    validate_bound_parent(&retirement.destination, &retirement.parent)?;
    if let Some(parent) = &retirement.publication_parent {
        validate_bound_parent(&retirement.destination, parent)?;
    }
    let invalid_discard = if retirement.discard {
        retirement.previous.is_some() || retirement.observed.is_some()
    } else {
        content == GeneratedContentAuthority::CurrentBuild && retirement.previous.is_none()
    };
    if invalid_discard
        || retirement
            .previous
            .as_ref()
            .is_some_and(|previous| !previous.validate())
        || retirement
            .observed
            .as_ref()
            .is_some_and(|observed| !observed.validate())
        || retirement
            .superseded
            .as_ref()
            .is_some_and(|superseded| !superseded.validate())
        || (retirement.observed.is_some() && retirement.superseded.is_some())
    {
        return Err(InstallError::InvalidRecord(
            "install retirement carries an invalid path or file image".to_owned(),
        ));
    }
    validate_introduced_parents(
        Location::CommonGit,
        &retirement.destination,
        retirement.previous.is_some(),
        &retirement.introduced_parents,
        introduced,
    )
}

/// An operation is bound to the install roots by what it records: a safe
/// relative path below the common directory with a mode this build installs.
/// A plan this build derives additionally names only assets it publishes, at
/// their contract path, with their contract mode and this build's content.
pub(super) fn validate_operation(
    operation: &Operation,
    introduced: &mut BTreeSet<(Location, String)>,
    content: GeneratedContentAuthority,
    common_git: &Path,
) -> Result<(), InstallError> {
    match (content, operation.asset.known()) {
        (GeneratedContentAuthority::CurrentBuild, Some(asset)) => {
            let (location, path, expected_mode) = asset_contract(asset)?;
            validate_operation_contract(operation, location, &path, expected_mode)?;
        }
        (GeneratedContentAuthority::CurrentBuild, None) => {
            return Err(InstallError::InvalidRecord(
                "install plan publishes an asset this build does not know".to_owned(),
            ));
        }
        (GeneratedContentAuthority::HistoricalReceipt, _) => {
            if ![DATA_MODE, EXECUTABLE_MODE].contains(&operation.generated.mode) {
                return Err(InstallError::InvalidRecord(
                    "install operation records a mode this build does not install".to_owned(),
                ));
            }
            validate_operation_contract(
                operation,
                operation.location,
                &operation.path,
                operation.generated.mode,
            )?;
        }
    }
    validate_generated_content(operation, content, common_git)?;
    validate_introduced_parents(
        operation.location,
        &operation.destination,
        operation.previous.is_some(),
        &operation.introduced_parents,
        introduced,
    )
}

fn validate_operation_contract(
    operation: &Operation,
    location: Location,
    path: &str,
    expected_mode: u32,
) -> Result<(), InstallError> {
    if operation.location != location
        || operation.path != path
        || operation.generated.mode != expected_mode
    {
        return Err(InstallError::InvalidRecord(format!(
            "install operation {:?} does not match its fixed contract",
            operation.asset
        )));
    }
    validate_relative(&operation.path)?;
    validate_relative(&operation.destination)?;
    validate_bound_parent(&operation.destination, &operation.parent)?;
    if let Some(parent) = &operation.publication_parent {
        validate_bound_parent(&operation.destination, parent)?;
    }
    if !operation.generated.validate()
        || operation
            .previous
            .as_ref()
            .is_some_and(|previous| !previous.validate())
        || operation
            .superseded
            .as_ref()
            .is_some_and(|superseded| !superseded.validate())
    {
        return Err(InstallError::InvalidRecord(
            "install operation carries an invalid file image".to_owned(),
        ));
    }
    Ok(())
}

fn validate_generated_content(
    operation: &Operation,
    content: GeneratedContentAuthority,
    common_git: &Path,
) -> Result<(), InstallError> {
    let Some(asset) = operation.asset.known() else {
        return Ok(());
    };
    if content == GeneratedContentAuthority::CurrentBuild
        && let Some(bytes) = fixed_asset_bytes(asset, common_git)?
        && operation.generated != GeneratedImage::new(asset_contract(asset)?.2, &bytes)
    {
        return Err(InstallError::InvalidRecord(format!(
            "{} differs from this build",
            asset_name(asset)
        )));
    }
    let empty = Digest::compute(Domain::InstallArtifact, &[]);
    match asset {
        Asset::Binary if operation.generated.digest == empty => {
            return Err(InstallError::InvalidRecord(
                "explicit Spice executable is empty".to_owned(),
            ));
        }
        Asset::SymbolsInfo | Asset::SymbolsDwarf if operation.generated.digest == empty => {
            return Err(InstallError::InvalidRecord(
                "retained symbol asset is empty".to_owned(),
            ));
        }
        _ => {}
    }
    Ok(())
}

/// Whether one recorded operation belongs to another Spice build: it names an
/// asset this build does not publish, publishes a known asset somewhere other
/// than its contract path, or carries different bytes for a fixed asset.
///
/// The binary's bytes are deliberately excluded: they come from the caller
/// rather than an embedded build asset, and [`super::rebuilt_binary`] compares
/// that explicit input.
pub(super) fn fixed_content_differs_from_current_build(
    operation: &Operation,
    common_git: &Path,
) -> Result<bool, InstallError> {
    let Some(asset) = operation.asset.known() else {
        return Ok(true);
    };
    let (location, path, mode) = asset_contract(asset)?;
    if operation.location != location || operation.path != path {
        return Ok(true);
    }
    let Some(bytes) = fixed_asset_bytes(asset, common_git)? else {
        return Ok(false);
    };
    Ok(operation.generated != GeneratedImage::new(mode, &bytes))
}

fn asset_name(asset: Asset) -> &'static str {
    match asset {
        Asset::GitPreCommit => "Git pre-commit hook",
        Asset::GitCommitMessage => "Git commit-msg hook",
        Asset::GitReferenceTransaction => "Git reference-transaction hook",
        Asset::GitHookConfig => "Git hook activation config",
        Asset::Binary => "Spice executable",
        Asset::SymbolsInfo => "retained dSYM Info.plist",
        Asset::SymbolsDwarf => "retained dSYM DWARF image",
    }
}

fn validate_bound_parent(
    destination: &str,
    parent: &ParentObservation,
) -> Result<(), InstallError> {
    if !parent.ancestor.is_empty() {
        validate_relative(&parent.ancestor)?;
    }
    if !parent.missing.is_empty() {
        validate_relative(&parent.missing)?;
    }
    let expected = Path::new(&parent.ancestor).join(&parent.missing);
    if Path::new(destination).parent() != Some(expected.as_path()) {
        return Err(InstallError::InvalidRecord(
            "install destination does not match its bound parent".to_owned(),
        ));
    }
    Ok(())
}

fn validate_introduced_parents(
    location: Location,
    destination: &str,
    has_previous: bool,
    owned: &[String],
    introduced: &mut BTreeSet<(Location, String)>,
) -> Result<(), InstallError> {
    let parents = super::path::relative_parents(destination)?;
    if has_previous && !owned.is_empty() {
        return Err(InstallError::InvalidRecord(
            "an existing target cannot own introduced parent directories".to_owned(),
        ));
    }
    let mut minimum_parent = 0;
    for relative in owned {
        validate_relative(relative)?;
        let Some(offset) = parents[minimum_parent..]
            .iter()
            .position(|parent| parent == relative)
        else {
            return Err(InstallError::InvalidRecord(
                "introduced directories are not canonical target parents".to_owned(),
            ));
        };
        minimum_parent += offset + 1;
        if !introduced.insert((location, relative.clone())) {
            return Err(InstallError::InvalidRecord(
                "introduced directory is duplicated across reversals".to_owned(),
            ));
        }
    }
    Ok(())
}

pub(super) fn asset_contract(asset: Asset) -> Result<(Location, String, u32), InstallError> {
    if let Some(path) = symbols::path(asset) {
        return Ok((Location::CommonGit, path.to_owned(), DATA_MODE));
    }
    let contract = match asset {
        Asset::GitPreCommit => (
            Location::CommonGit,
            GIT_PRE_COMMIT_PATH.to_owned(),
            EXECUTABLE_MODE,
        ),
        Asset::GitCommitMessage => (
            Location::CommonGit,
            GIT_COMMIT_MESSAGE_PATH.to_owned(),
            EXECUTABLE_MODE,
        ),
        Asset::GitReferenceTransaction => (
            Location::CommonGit,
            GIT_REFERENCE_TRANSACTION_PATH.to_owned(),
            EXECUTABLE_MODE,
        ),
        Asset::GitHookConfig => (
            Location::CommonGit,
            GIT_HOOK_CONFIG_PATH.to_owned(),
            DATA_MODE,
        ),
        Asset::Binary => (Location::CommonGit, BINARY_PATH.to_owned(), EXECUTABLE_MODE),
        Asset::SymbolsInfo | Asset::SymbolsDwarf => unreachable!("symbol assets returned above"),
    };
    Ok(contract)
}
