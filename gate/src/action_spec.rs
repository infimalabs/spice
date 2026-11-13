//! Canonical specifications and authority-bound identities for reusable gate actions.

use serde::{Deserialize, Serialize};
use wire::{Digest, Domain};

use crate::{ActionInputError, ActionKey, ActionReuseClass, derive_action_key};

/// Current reusable gate action specification schema.
pub const ACTION_SPEC_VERSION: u64 = 1;

/// Current reusable gate action-key document schema.
pub const ACTION_KEY_VERSION: u64 = 1;

/// One executable required by a canonical action.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionToolRequirement {
    name: String,
    program: String,
}

impl ActionToolRequirement {
    /// Bind a declaration name to its exact executable lookup name.
    pub fn new(
        name: impl Into<String>,
        program: impl Into<String>,
    ) -> Result<Self, ActionInputError> {
        let requirement = Self {
            name: name.into(),
            program: program.into(),
        };
        validate_text("action tool name", &requirement.name)?;
        validate_text("action tool program", &requirement.program)?;
        Ok(requirement)
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }
}

/// Fields supplied by the runner when constructing one stable action specification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionSpecFields {
    pub action_id: String,
    pub runner: String,
    pub runner_version: u64,
    pub declaration: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub environment_names: Vec<String>,
    pub tools: Vec<ActionToolRequirement>,
    pub input_strategy: String,
    pub reuse: ActionReuseClass,
}

/// Stable execution contract for one reusable gate action.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSpec {
    version: u64,
    action_id: String,
    runner: String,
    runner_version: u64,
    declaration: String,
    argv: Vec<String>,
    cwd: String,
    environment_names: Vec<String>,
    tools: Vec<ActionToolRequirement>,
    input_strategy: String,
    reuse: ActionReuseClass,
}

impl ActionSpec {
    /// Construct and validate one current action specification.
    pub fn new(fields: ActionSpecFields) -> Result<Self, ActionInputError> {
        let spec = Self {
            version: ACTION_SPEC_VERSION,
            action_id: fields.action_id,
            runner: fields.runner,
            runner_version: fields.runner_version,
            declaration: fields.declaration,
            argv: fields.argv,
            cwd: fields.cwd,
            environment_names: fields.environment_names,
            tools: fields.tools,
            input_strategy: fields.input_strategy,
            reuse: fields.reuse,
        };
        spec.validate()?;
        Ok(spec)
    }

    #[must_use]
    pub fn action_id(&self) -> &str {
        &self.action_id
    }

    /// Canonical JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ActionInputError> {
        self.validate()?;
        wire::encode(self).map_err(|error| ActionInputError::new(error.to_string()))
    }

    /// Domain-separated identity of this execution contract.
    pub fn digest(&self) -> Result<Digest, ActionInputError> {
        Ok(Digest::compute(Domain::GateActionSpec, &self.encode()?))
    }

    fn validate(&self) -> Result<(), ActionInputError> {
        if self.version != ACTION_SPEC_VERSION || self.runner_version == 0 {
            return Err(ActionInputError::new(
                "action spec requires the current schema and a nonzero runner version",
            ));
        }
        for (field, value) in [
            ("action id", &self.action_id),
            ("runner", &self.runner),
            ("declaration", &self.declaration),
            ("working directory", &self.cwd),
            ("input strategy", &self.input_strategy),
        ] {
            validate_text(field, value)?;
        }
        if self.argv.is_empty() || self.argv.iter().any(|value| invalid_text(value)) {
            return Err(ActionInputError::new(
                "action argv must contain only nonempty single-line values",
            ));
        }
        require_sorted_unique("action environment names", &self.environment_names)?;
        if self.tools.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(ActionInputError::new(
                "action tool requirements must be sorted and unique",
            ));
        }
        Ok(())
    }
}

/// Runtime authority identities that bind one action key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionKeyAuthority {
    pub input_manifest: Digest,
    pub policy_authority: ActionPolicyAuthority,
    pub gate_set: Digest,
    pub tools: Digest,
    pub environment: Digest,
    pub baseline: Option<Digest>,
}

/// How policy affects one reusable action's execution identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionPolicyAuthority {
    /// The action reads complete policy inputs retained in its input manifest.
    PolicyConsuming,
    /// Policy selection has already been reduced to the action's resolved inputs.
    ResolvedExecution,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ActionKeyDocument<'a> {
    version: u64,
    action_spec: &'a Digest,
    input_manifest: &'a Digest,
    policy_authority: ActionPolicyAuthority,
    gate_set: &'a Digest,
    tools: &'a Digest,
    environment: &'a Digest,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline: Option<&'a Digest>,
}

/// Derive the reusable identity only from a validated spec and complete authorities.
pub fn derive_authority_bound_action_key(
    spec: &ActionSpec,
    authority: &ActionKeyAuthority,
) -> Result<ActionKey, ActionInputError> {
    let action_spec = spec.digest()?;
    let document = ActionKeyDocument {
        version: ACTION_KEY_VERSION,
        action_spec: &action_spec,
        input_manifest: &authority.input_manifest,
        policy_authority: authority.policy_authority,
        gate_set: &authority.gate_set,
        tools: &authority.tools,
        environment: &authority.environment,
        baseline: authority.baseline.as_ref(),
    };
    let bytes =
        wire::encode(&document).map_err(|error| ActionInputError::new(error.to_string()))?;
    Ok(derive_action_key(&bytes))
}

fn require_sorted_unique(label: &str, values: &[String]) -> Result<(), ActionInputError> {
    if values.iter().any(|value| invalid_text(value))
        || values.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(ActionInputError::new(format!(
            "{label} must be sorted, unique, and nonempty"
        )));
    }
    Ok(())
}

fn validate_text(label: &str, value: &str) -> Result<(), ActionInputError> {
    if invalid_text(value) {
        return Err(ActionInputError::new(format!("{label} is invalid")));
    }
    Ok(())
}

fn invalid_text(value: &str) -> bool {
    value.is_empty() || value.trim() != value || value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(seed: &[u8]) -> Digest {
        Digest::compute(Domain::GateInventory, seed)
    }

    fn spec() -> ActionSpec {
        ActionSpec::new(ActionSpecFields {
            action_id: "gate:formatters".to_owned(),
            runner: "spice-focused-gate".to_owned(),
            runner_version: 1,
            declaration: "gate::toolchain::formatters::toolchain-v1".to_owned(),
            argv: vec!["gate".to_owned(), "formatters".to_owned()],
            cwd: ".".to_owned(),
            environment_names: vec!["PATH".to_owned()],
            tools: vec![ActionToolRequirement::new("rustfmt", "rustfmt").unwrap()],
            input_strategy: "selected-reach-v1".to_owned(),
            reuse: ActionReuseClass::Cacheable,
        })
        .unwrap()
    }

    #[test]
    fn action_spec_and_key_are_canonical_and_authority_sensitive() {
        let spec = spec();
        assert_eq!(spec.digest().unwrap(), spec.digest().unwrap());
        let authority = ActionKeyAuthority {
            input_manifest: digest(b"manifest"),
            policy_authority: ActionPolicyAuthority::ResolvedExecution,
            gate_set: digest(b"gate-set"),
            tools: digest(b"tools"),
            environment: digest(b"environment"),
            baseline: None,
        };
        let original = derive_authority_bound_action_key(&spec, &authority).unwrap();
        let moved_authorities = [
            ActionKeyAuthority {
                input_manifest: digest(b"moved-manifest"),
                ..authority.clone()
            },
            ActionKeyAuthority {
                policy_authority: ActionPolicyAuthority::PolicyConsuming,
                ..authority.clone()
            },
            ActionKeyAuthority {
                gate_set: digest(b"moved-gate-set"),
                ..authority.clone()
            },
            ActionKeyAuthority {
                tools: digest(b"moved-tools"),
                ..authority.clone()
            },
            ActionKeyAuthority {
                environment: digest(b"moved-environment"),
                ..authority.clone()
            },
            ActionKeyAuthority {
                baseline: Some(digest(b"moved-baseline")),
                ..authority.clone()
            },
        ];
        for moved in moved_authorities {
            assert_ne!(
                original,
                derive_authority_bound_action_key(&spec, &moved).unwrap()
            );
        }

        let action_spec = spec.digest().unwrap();
        let document = ActionKeyDocument {
            version: ACTION_KEY_VERSION,
            action_spec: &action_spec,
            input_manifest: &authority.input_manifest,
            policy_authority: authority.policy_authority,
            gate_set: &authority.gate_set,
            tools: &authority.tools,
            environment: &authority.environment,
            baseline: authority.baseline.as_ref(),
        };
        let encoded = wire::encode(&document).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["version"], ACTION_KEY_VERSION);
        assert_eq!(value["policy_authority"], "resolved-execution");
        assert!(value.get("policy").is_none());
    }

    #[test]
    fn action_spec_refuses_ambiguous_ordering() {
        let mut fields = ActionSpecFields {
            action_id: "gate:taste".to_owned(),
            runner: "spice-focused-gate".to_owned(),
            runner_version: 1,
            declaration: "taste-v1".to_owned(),
            argv: vec!["gate".to_owned(), "taste".to_owned()],
            cwd: ".".to_owned(),
            environment_names: vec!["Z".to_owned(), "A".to_owned()],
            tools: Vec::new(),
            input_strategy: "selected-reach-v1".to_owned(),
            reuse: ActionReuseClass::Cacheable,
        };
        assert!(ActionSpec::new(fields.clone()).is_err());
        fields.environment_names = vec!["PATH".to_owned()];
        fields.tools = vec![
            ActionToolRequirement::new("z", "z").unwrap(),
            ActionToolRequirement::new("a", "a").unwrap(),
        ];
        assert!(ActionSpec::new(fields).is_err());
    }
}
