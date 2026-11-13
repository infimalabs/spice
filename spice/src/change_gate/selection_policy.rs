use super::*;

pub(super) struct PackagedPolicy {
    pub(super) selection: gate::ChangeSelectionPolicy,
    pub(super) test_engines: Vec<crate::config::TestEngine>,
}
pub(super) struct CurrentSourcePolicy {
    pub(super) rules: gate::RulePolicy,
    pub(super) generated_paths: Vec<String>,
}

#[cfg(test)]
pub(super) fn packaged_selection_policy(
    root: &Path,
    tracked_sources: &[String],
    environment: &Environment,
) -> Result<gate::ChangeSelectionPolicy, ChangeGateError> {
    packaged_policy(root, tracked_sources, environment).map(|policy| policy.selection)
}

pub(super) fn packaged_policy(
    root: &Path,
    tracked_sources: &[String],
    environment: &Environment,
) -> Result<PackagedPolicy, ChangeGateError> {
    let tracked = tracked_policy(root)?;
    let document = tracked
        .as_deref()
        .map(|text| crate::config::Document::new(wire::TRACKED_POLICY_FILE, text));
    let resolved = crate::config::resolve(crate::config::Inputs::new(document, None, &[]))
        .map_err(|error| ChangeGateError::refusal("change-selection-policy-refused", error))?;
    let policy = resolved.root().policy();
    let declared_rust_sources = gate::cargo_declared_compile_time_sources_at(root, environment)
        .map_err(|error| {
            ChangeGateError::refusal("change-selection-cargo-source-refused", error)
        })?;
    let rules = compile_rule_policy(tracked.as_deref())?;
    let selection = gate::ChangeSelectionPolicy::new(gate::ChangeSelectionPolicyInputs {
        tracked_sources,
        policy_authorities: &[wire::TRACKED_POLICY_FILE.to_owned()],
        configured: tracked.is_some(),
        rules: &rules,
        file_shape_suffixes: policy.file_shape().source_suffixes(),
        complexity_suffixes: policy.languages().complexity(),
        magic_suffixes: policy.languages().magic(),
        package_roots: policy.package_roots(),
        declared_rust_sources: &declared_rust_sources,
        generated_paths: policy.generated_paths(),
        test_paths: policy.test_paths(),
    })
    .map_err(|error| ChangeGateError::refusal("change-selection-policy-refused", error))?;
    Ok(PackagedPolicy {
        selection,
        test_engines: policy.test_engines().to_vec(),
    })
}

pub(super) fn current_source_policy(root: &Path) -> Result<CurrentSourcePolicy, ChangeGateError> {
    let tracked = tracked_policy(root)?;
    let document = tracked
        .as_deref()
        .map(|text| crate::config::Document::new(wire::TRACKED_POLICY_FILE, text));
    let resolved = crate::config::resolve(crate::config::Inputs::new(document, None, &[]))
        .map_err(|error| ChangeGateError::refusal("change-selection-policy-refused", error))?;
    let generated_paths = resolved.root().policy().generated_paths().to_vec();
    let rules = compile_rule_policy(tracked.as_deref())?;
    Ok(CurrentSourcePolicy {
        rules,
        generated_paths,
    })
}

fn tracked_policy(root: &Path) -> Result<Option<String>, ChangeGateError> {
    let path = root.join(wire::TRACKED_POLICY_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ChangeGateError::environment(
            "change-selection-policy-unavailable",
            format!("{}: {error}", path.display()),
        )),
    }
}

fn compile_rule_policy(tracked: Option<&str>) -> Result<gate::RulePolicy, ChangeGateError> {
    gate::repository_rule_policy(tracked)
        .map_err(|error| ChangeGateError::refusal("change-selection-policy-refused", error))
}
