use super::*;

fn journal_schema_requirements(
    written: &str,
) -> Result<wire::EnvironmentRequirements, Outcome<CommandAnswer>> {
    let value = written.parse::<u32>().map_err(|_| {
        refusal(
            "task::journal-schema-invalid",
            Some(written.to_owned()),
            "name one canonical positive decimal journal schema",
        )
    })?;
    if value.to_string() != written {
        return Err(refusal(
            "task::journal-schema-invalid",
            Some(written.to_owned()),
            "name one canonical positive decimal journal schema",
        ));
    }
    let requirement = wire::EnvironmentRequirement::journal_schema(value).map_err(|error| {
        refusal(
            "task::journal-schema-invalid",
            Some(written.to_owned()),
            error.to_string(),
        )
    })?;
    Ok(wire::EnvironmentRequirements::one(requirement))
}

fn resource_pool_requirement(
    written: &str,
) -> Result<wire::EnvironmentRequirement, Outcome<CommandAnswer>> {
    let (written_name, written_shards) = written.split_once('=').unwrap_or((written, "1"));
    let name = written_name
        .parse::<wire::ResourcePoolName>()
        .map_err(|error| {
            refusal(
                "task::resource-name-invalid",
                Some(written_name.to_owned()),
                error.to_string(),
            )
        })?;
    let count = written_shards.parse::<u32>().map_err(|_| {
        refusal(
            "task::resource-shards-invalid",
            Some(written_shards.to_owned()),
            "name one canonical positive decimal shard count",
        )
    })?;
    if count.to_string() != written_shards {
        return Err(refusal(
            "task::resource-shards-invalid",
            Some(written_shards.to_owned()),
            "name one canonical positive decimal shard count",
        ));
    }
    wire::EnvironmentRequirement::resource_pool(name, count).map_err(|error| {
        refusal(
            "task::resource-shards-invalid",
            Some(written_shards.to_owned()),
            error.to_string(),
        )
    })
}

pub(super) fn appliance_version(
    written: &str,
) -> Result<wire::manifest::ProductVersion, Outcome<CommandAnswer>> {
    wire::manifest::ProductVersion::parse(written).map_err(|_| {
        refusal(
            "task::appliance-version-invalid",
            Some(written.to_owned()),
            "name one canonical semantic version without a v prefix",
        )
    })
}

pub(super) fn environment_requirements(
    schema: Option<&str>,
    trust: Option<&str>,
    appliance: Option<wire::manifest::ProductVersion>,
    resources: &[&str],
) -> Result<wire::EnvironmentRequirements, Outcome<CommandAnswer>> {
    let mut requirements = Vec::new();
    if let Some(written) = schema {
        requirements.extend_from_slice(journal_schema_requirements(written)?.as_slice());
    }
    if let Some(written) = trust {
        let digest = written.parse::<wire::Digest>().map_err(|_| {
            refusal(
                "task::trust-grant-invalid",
                Some(written.to_owned()),
                "name one canonical lowercase 64-character executable-configuration digest",
            )
        })?;
        requirements.push(wire::EnvironmentRequirement::trust_grant(digest));
    }
    if let Some(version) = appliance {
        requirements.push(wire::EnvironmentRequirement::appliance_after(version));
    }
    let mut declared = resources
        .iter()
        .map(|written| resource_pool_requirement(written))
        .collect::<Result<Vec<_>, _>>()?;
    declared.sort_by(|left, right| match (left, right) {
        (
            wire::EnvironmentRequirement::ResourcePool { name: left, .. },
            wire::EnvironmentRequirement::ResourcePool { name: right, .. },
        ) => left.cmp(right),
        _ => unreachable!("resource parser returns resource requirements"),
    });
    requirements.extend(declared);
    wire::EnvironmentRequirements::new(requirements).map_err(|error| {
        refusal(
            "task::requirements-noncanonical",
            None,
            format!("resource declarations must be distinct: {error}"),
        )
    })
}

pub(super) fn witnesses(board: &BoardAnswer, heads: &BoardTask) -> Vec<Witness> {
    let mut expected = vec![statement_witness(heads), claim_witness(heads)];
    expected.extend(
        heads
            .held_by(&board.principal)
            .and_then(|held| held.lease_witness(&heads.statement.body.born)),
    );
    expected
}

pub(super) fn client_require(
    repository: Host<'_>,
    contract: &Contract,
    matches: &ArgMatches,
) -> Outcome<CommandAnswer> {
    let written = match matches
        .get_one::<String>("appliance-after")
        .map(|written| appliance_version(written))
        .transpose()
    {
        Ok(written) => written,
        Err(refused) => return refused,
    };
    mutate_task_from_board(repository, contract, matches, |board, task, heads| {
        let schema = matches.get_one::<String>("journal-schema");
        let trust = matches.get_one::<String>("trust-grant");
        // A bare `--appliance-after` names the version the resident runs now.
        let appliance = matches
            .contains_id("appliance-after")
            .then(|| written.unwrap_or_else(|| board.appliance.clone()));
        let resources = matches
            .get_many::<String>("resource")
            .map(|values| values.map(String::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        let clear = matches.get_flag("clear");
        let named =
            schema.is_some() || trust.is_some() || appliance.is_some() || !resources.is_empty();
        let mutable = match (named, clear) {
            (true, false) => environment_requirements(
                schema.map(String::as_str),
                trust.map(String::as_str),
                appliance,
                &resources,
            )?,
            (false, true) => wire::EnvironmentRequirements::default(),
            (true, true) => {
                return Err(refusal(
                    "task::require-shape",
                    None,
                    "name environmental requirements or --clear, never both",
                ));
            }
            (false, false) => {
                return Err(refusal(
                    "task::require-shape",
                    None,
                    "name --journal-schema SCHEMA, --trust-grant DIGEST, \
                     --appliance-after [VERSION], and/or --resource POOL[=COUNT] \
                     to replace task requirements, or --clear to remove them",
                ));
            }
        };
        let requirements = if mutable.is_empty() {
            wire::intent::TaskRequirementsEdit::Clear {}
        } else {
            wire::intent::TaskRequirementsEdit::Set {
                requirements: mutable,
            }
        };
        Ok((
            CommandKind::TaskEdit,
            witnesses(board, heads),
            serde_json::to_value(TaskEdit {
                intent: None,
                task,
                title: None,
                pitch: None,
                description: None,
                term: None,
                requirements: Some(requirements),
                priority: None,
                topic: None,
                flow: None,
            })
            .expect("typed intent encodes"),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_schema_cli_values_are_positive_canonical_decimals() {
        let maximum = u32::MAX.to_string();
        for admitted in ["1", "15", maximum.as_str()] {
            assert!(
                journal_schema_requirements(admitted).is_ok(),
                "{admitted:?} is canonical"
            );
        }
        for refused in ["", "0", "01", "+1", "-1", "1.0", "4294967296"] {
            assert!(
                journal_schema_requirements(refused).is_err(),
                "{refused:?} must not normalize"
            );
        }
    }

    #[test]
    fn trust_grant_cli_values_are_canonical_and_join_schema_in_fact_order() {
        let digest = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let requirements = environment_requirements(Some("18"), Some(digest), None, &[])
            .expect("canonical conjunction");
        assert!(matches!(
            requirements.as_slice(),
            [
                wire::EnvironmentRequirement::JournalSchema { .. },
                wire::EnvironmentRequirement::TrustGrant { .. }
            ]
        ));
        for refused in [
            "",
            "abc",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let outcome = environment_requirements(None, Some(refused), None, &[])
                .expect_err("noncanonical digest refuses");
            assert!(matches!(
                outcome,
                Outcome::Refusal(ref refusal)
                    if refusal.reason() == "task::trust-grant-invalid"
            ));
        }
    }
    #[test]
    fn resource_pool_cli_values_are_canonical_and_positive() {
        let requirements =
            environment_requirements(None, None, None, &["windows-guest", "emulator=2"])
                .expect("canonical resource requirements");
        let resources = requirements.compiled_resources();
        let actual = resources
            .iter()
            .map(|(name, shards)| (name.as_str(), shards))
            .collect::<Vec<_>>();
        assert_eq!(actual, [("emulator", 2), ("windows-guest", 1)]);

        for refused in [
            "../guest",
            "windows-guest=0",
            "windows-guest=02",
            "windows-guest=",
            "=1",
        ] {
            assert!(
                environment_requirements(None, None, None, &[refused]).is_err(),
                "{refused:?} must refuse"
            );
        }
        assert!(
            environment_requirements(None, None, None, &["windows-guest", "windows-guest=2"],)
                .is_err(),
            "duplicate pools must refuse"
        );
    }

    #[test]
    fn appliance_version_cli_values_are_canonical_semver_refused_by_reason() {
        let written = "0.2.0-dev.0.dTYp0bz3B+bin.abc";
        let version = appliance_version(written).expect("canonical version");
        assert_eq!(version.as_str(), written);
        for refused in [
            "",
            "v0.2.0",
            "0.2",
            "01.2.0",
            "0.2.0-dev.00",
            "0.2.0-",
            "0.2.0+",
        ] {
            let outcome = appliance_version(refused).expect_err("noncanonical version refuses");
            assert!(
                matches!(
                    &outcome,
                    Outcome::Refusal(refusal)
                        if refusal.reason() == "task::appliance-version-invalid"
                ),
                "{refused:?} must refuse by reason"
            );
        }
    }
}
