//! Exact command-rim capture assembly for complete and focused gate runs.

use super::*;

/// Capture at the ambient working directory.
pub(super) fn capture_inner<'trace>(
    ambient: &Ambient,
    selector: Selector,
    input_mode: InputMode,
    selected: Option<GateId>,
    trace: Option<&'trace mut (dyn FnMut(RepositoryExecutionEvent) + Send)>,
    claim: ClaimCapture,
) -> Result<BatteryCapture<'trace>, RunError> {
    let directory = env::current_dir()
        .map_err(|error| RunError::fault("gate::working-directory-unavailable", error))?;
    capture_at_inner(
        ambient, &directory, selector, input_mode, selected, trace, claim,
    )
}

/// Capture at an explicitly admitted repository directory.
pub(super) fn capture_at_inner<'trace>(
    ambient: &Ambient,
    directory: &Path,
    selector: Selector,
    input_mode: InputMode,
    selected: Option<GateId>,
    trace: Option<&'trace mut (dyn FnMut(RepositoryExecutionEvent) + Send)>,
    claim: ClaimCapture,
) -> Result<BatteryCapture<'trace>, RunError> {
    let repository = Repository::discover(directory)
        .map_err(|error| RunError::fault(crate::repository::ResolveError::REASON, error))?;
    let state = repository.admit_state().map_err(|error| match error {
        StateError::Filesystem { .. } => RunError::fault("gate::state-unavailable", error),
        _ => RunError::refusal("gate::state-refused", error),
    })?;
    let covers = super::sweep::rim_covers(&state, ambient)?;
    let actor = acting_actor(repository.worktree_root())?;
    let search_path = ambient.get(PROCESS_PATH).map(OsStr::to_os_string);
    let rim = match &selector {
        Selector::Staged => RimCapture::staged(directory, search_path),
        Selector::Revision(_) => RimCapture::new(
            directory,
            search_path.clone(),
            selected_changes(
                repository.worktree_root(),
                &selector,
                search_path.as_deref(),
            )?,
        ),
    }
    .map_err(|error| RunError::refusal("gate::rim-refused", error))?;
    let root = repository.worktree_root();
    let actor = ActorCapture::Acting {
        actor,
        // The shared state run owns the exact clock and active claims.
        bound_claims: Vec::new(),
    };
    let hygiene = HygieneConfiguration::current();
    BatteryCapture::capture(
        CaptureAuthorities {
            repository: root.to_path_buf(),
            selector: &selector,
            claim,
            actor,
            rim,
            hygiene,
            covers,
        },
        input_mode,
        selected,
        trace,
    )
    .map_err(|error| {
        let detail = error.detail();
        let reason = if detail.starts_with("cannot load captured battery contract:") {
            "gate::contract-refused"
        } else if detail.starts_with("cannot capture battery source:") {
            "gate::selection-refused"
        } else {
            "gate::capture-refused"
        };
        RunError::refusal(reason, detail)
    })
}
