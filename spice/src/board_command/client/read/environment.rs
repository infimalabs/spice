use super::*;

pub(super) fn requirement_lines(
    board: &BoardAnswer,
    statement: &wire::intent::BoardStatement,
    environment: &wire::Environment,
) -> Vec<String> {
    let mut lines = vec!["requirements:".to_owned()];
    if statement.requirements.is_empty() {
        lines.push("  -".to_owned());
    }
    for requirement in statement.requirements.as_slice() {
        match requirement {
            wire::EnvironmentRequirement::JournalSchema { value: required } => {
                let observed = board.schema;
                let observation = wire::EnvironmentObservation::journal_schema(observed)
                    .expect("an admitted journal schema is positive");
                lines.push(format!(
                    "  - journal-schema required={} observed={observed} satisfied={}",
                    required,
                    requirement.is_satisfied_by(&observation),
                ));
            }
            wire::EnvironmentRequirement::TrustGrant { value: required } => {
                let observed = board
                    .trust
                    .iter()
                    .find(|head| head.body.digest == *required)
                    .map(|head| &head.body);
                let standing = observed.map_or("missing", |head| head.state.as_str());
                let producer = observed
                    .and_then(|head| head.worktree.as_deref())
                    .unwrap_or("-");
                let satisfied = wire::EnvironmentRequirements::new(vec![requirement.clone()])
                    .expect("one admitted requirement is canonical")
                    .are_satisfied_by(environment);
                lines.push(format!(
                    "  - trust-grant required={required} standing={standing} producer={producer} satisfied={satisfied}"
                ));
            }
            wire::EnvironmentRequirement::ApplianceVersion { value: required } => {
                let observation = wire::EnvironmentObservation::ApplianceVersion {
                    value: board.appliance.clone(),
                };
                lines.push(format!(
                    "  - appliance-version after={required} observed={} satisfied={}",
                    board.appliance,
                    requirement.is_satisfied_by(&observation),
                ));
            }
            wire::EnvironmentRequirement::ResourcePool { name, shards } => {
                lines.push(format!(
                    "  - resource-pool name={name} shards={shards} standing=uncompiled"
                ));
            }
        }
    }
    lines.push("resources:".to_owned());
    if statement.resources.is_empty() {
        lines.push("  -".to_owned());
    } else {
        lines.extend(statement.resources.iter().map(|(name, shards)| {
            let available = board.resource_pools.get(name).copied().unwrap_or(0);
            format!(
                "  - {name} shards={shards} available={available} satisfied={}",
                available >= shards
            )
        }));
    }
    lines
}

pub(super) struct EnvironmentStatus {
    pub(super) satisfied: usize,
    pub(super) blocked: usize,
    pub(super) schemas: String,
    pub(super) trust: String,
    pub(super) trust_observed: String,
    pub(super) trust_satisfied: String,
    pub(super) appliance: String,
    pub(super) resources: String,
}

pub(super) fn environment_status(
    rows: &[&Entry],
    environment: &wire::Environment,
) -> EnvironmentStatus {
    let blocked = rows
        .iter()
        .filter(|entry| !board::environment_satisfied(entry, environment))
        .count();
    let mut schemas = Vec::new();
    let mut trust = Vec::new();
    let mut trust_observed = Vec::new();
    let mut trust_satisfied = Vec::new();
    let mut appliance = Vec::new();
    let mut resources = Vec::new();
    for requirement in rows
        .iter()
        .flat_map(|entry| entry.statement().requirements.as_slice())
    {
        match requirement {
            wire::EnvironmentRequirement::JournalSchema { value } => schemas.push(value.get()),
            wire::EnvironmentRequirement::TrustGrant { value } => {
                trust.push(value.to_string());
                let standing = environment
                    .trust_head(value)
                    .map_or("missing", |head| head.state.as_str());
                trust_observed.push(format!("{value}:{standing}"));
                trust_satisfied.push(format!("{value}:{}", environment.satisfies(requirement)));
            }
            wire::EnvironmentRequirement::ApplianceVersion { value } => appliance.push(value),
            wire::EnvironmentRequirement::ResourcePool { name, shards } => {
                resources.push(format!("{name}:{}", shards.get()));
            }
        }
    }
    schemas.sort_unstable();
    schemas.dedup();
    trust.sort();
    trust.dedup();
    trust_observed.sort();
    trust_observed.dedup();
    trust_satisfied.sort();
    trust_satisfied.dedup();
    appliance.sort_by(|left, right| {
        left.precedence(right)
            .then_with(|| left.as_str().cmp(right.as_str()))
    });
    appliance.dedup();
    resources.sort();
    resources.dedup();
    EnvironmentStatus {
        satisfied: rows.len() - blocked,
        blocked,
        schemas: listed(schemas.into_iter().map(|value| value.to_string())),
        trust: listed(trust.into_iter()),
        trust_observed: listed(trust_observed.into_iter()),
        trust_satisfied: listed(trust_satisfied.into_iter()),
        appliance: listed(appliance.into_iter().map(ToString::to_string)),
        resources: listed(resources.into_iter()),
    }
}

fn listed(values: impl Iterator<Item = String>) -> String {
    let joined = values.collect::<Vec<_>>().join(",");
    if joined.is_empty() {
        "-".to_owned()
    } else {
        joined
    }
}
