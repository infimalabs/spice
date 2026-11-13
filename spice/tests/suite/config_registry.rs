use std::collections::BTreeSet;

use spice::config::{Builtin, PathSegment, PolicyClass, Root, ValueKind, descriptor, descriptors};
use toml::Value;
use wire::ConfigSource;

fn decode(document: &str) -> Result<Root, String> {
    let deserializer = toml::Deserializer::parse(document).map_err(|error| error.to_string())?;
    serde_path_to_error::deserialize(deserializer).map_err(|error| error.to_string())
}

fn encoded(root: &Root) -> Value {
    let document = toml::to_string(root).expect("the typed root encodes");
    toml::from_str(&document).expect("encoded root is TOML")
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum OwnedSegment {
    Key(String),
    Index(usize),
}

fn borrowed(path: &[OwnedSegment]) -> Vec<PathSegment<'_>> {
    path.iter()
        .map(|segment| match segment {
            OwnedSegment::Key(key) => PathSegment::Key(key),
            OwnedSegment::Index(index) => PathSegment::Index(*index),
        })
        .collect()
}

fn leaves(value: &Value) -> Vec<(Vec<OwnedSegment>, Value)> {
    fn walk(
        path: &mut Vec<OwnedSegment>,
        value: &Value,
        output: &mut Vec<(Vec<OwnedSegment>, Value)>,
    ) {
        match value {
            Value::Table(table) => {
                for (field, child) in table {
                    path.push(OwnedSegment::Key(field.clone()));
                    walk(path, child, output);
                    path.pop();
                }
            }
            Value::Array(values) if values.iter().all(Value::is_table) && !values.is_empty() => {
                for (index, child) in values.iter().enumerate() {
                    path.push(OwnedSegment::Index(index));
                    walk(path, child, output);
                    path.pop();
                }
            }
            Value::Array(values) if values.is_empty() && descriptor(&borrowed(path)).is_none() => {}
            _ => output.push((path.clone(), value.clone())),
        }
    }

    let mut output = Vec::new();
    walk(&mut Vec::new(), value, &mut output);
    output
}

fn builtin_matches(builtin: Builtin, value: &Value) -> bool {
    match (builtin, value) {
        (Builtin::String(expected), Value::String(actual)) => actual == expected,
        (Builtin::Strings(expected), Value::Array(actual)) => {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| actual.as_str() == Some(expected))
        }
        (Builtin::UnsignedInteger(expected), Value::Integer(actual)) => {
            u64::try_from(*actual) == Ok(expected)
        }
        (Builtin::Float(expected), Value::Float(actual)) => actual == &expected,
        (Builtin::Boolean(expected), Value::Boolean(actual)) => actual == &expected,
        _ => false,
    }
}

#[test]
fn shared_metadata_directly_refuses_invalid_names_depths_and_children() {
    for (document, expected) in [
        ("[policy]\nenv_names = [\"\"]\n", "nonempty"),
        (
            "[policy]\nenv_names = [\"FIXTURE_PROOF\", \"FIXTURE_PROOF\"]\n",
            "unique",
        ),
        ("[tasks]\nunknown = true\n", "unknown"),
        ("[wrappers]\nunknown = \"appliance\"\n", "unknown"),
        ("[wrappers]\nspice = \"ambient\"\n", "unknown variant"),
        ("[rtk]\nexecutable = \"\"\n", "non-empty executable"),
        ("[rtk]\nexecutable = \"path/rtk\"\n", "basename or absolute"),
        ("[rtk]\nunknown = \"rtk\"\n", "unknown"),
        ("[policy]\nunknown = true\n", "unknown"),
    ] {
        let error = decode(document).expect_err("invalid shared metadata must refuse");
        assert!(error.contains(expected), "{document:?}: {error}");
    }
}

#[test]
fn pre_commit_commands_admit_only_the_current_nonempty_shape() {
    for document in [
        "[policy]\npre_commit = [{ label = \"\", run = [\"gate/pre-commit\"] }]\n",
        "[policy]\npre_commit = [{ label = \"  \", run = [\"gate/pre-commit\"] }]\n",
        "[policy]\npre_commit = [{ label = \"Rust gate\", run = [] }]\n",
        "[policy]\npre_commit = [{ label = \"Rust gate\", run = [\"\"] }]\n",
        "[policy]\npre_commit = [{ label = \"Rust gate\", run = [\"gate/pre-commit\"], scopes = { paths = [\"\"] } }]\n",
        "[policy]\npre_commit = [{ label = \"Rust gate\", run = [\"gate/pre-commit\"], scopes = { paths = [\"src/**\", \"src/**\"] } }]\n",
        "[policy]\npre_commit = [{ label = \"Rust gate\", run = [\"gate/pre-commit\"], scopes = { unknown = [\"src/**\"] } }]\n",
        "[policy]\npre_commit = [{ label = \"Rust gate\", run = [\"gate/pre-commit\"], scopes = { paths = [\"../src/**\"] } }]\n",
    ] {
        assert!(decode(document).is_err(), "{document:?} must refuse");
    }

    assert!(decode("[policy]\npre_commit_builtins = false\n").is_err());
}

#[test]
fn arbitrary_pre_commit_commands_round_trip_in_authored_order() {
    let document = r#"
[policy]
pre_commit_builtins = true
pre_commit = [
    { label = "first", run = ["tool", "one"] },
    { label = "second", run = ["tool", "two"], scopes = { paths = ["src/**"] } },
    { label = "third", run = ["tool", "three", "--exact"] },
]
"#;
    let root = decode(document).expect("an arbitrary ordered list decodes");
    let round_trip = decode(&toml::to_string(&root).expect("ordered policy serializes"))
        .expect("serialized ordered policy decodes");
    assert_eq!(round_trip, root);
    assert_eq!(
        root.policy()
            .pre_commit()
            .iter()
            .map(|step| {
                (
                    step.label(),
                    step.run().join("\0"),
                    step.scopes().paths().join("\0"),
                )
            })
            .collect::<Vec<_>>(),
        [
            ("first", "tool\0one".to_owned(), String::new()),
            ("second", "tool\0two".to_owned(), "src/**".to_owned(),),
            ("third", "tool\0three\0--exact".to_owned(), String::new()),
        ]
    );
}

#[test]
fn registry_builtins_are_the_typed_root_defaults() {
    let mut seen = BTreeSet::new();
    for (path, value) in leaves(&encoded(&Root::default())) {
        let entry =
            descriptor(&borrowed(&path)).unwrap_or_else(|| panic!("unregistered default {path:?}"));
        let builtin = entry
            .builtin()
            .unwrap_or_else(|| panic!("default {path:?} has no builtin metadata"));
        assert!(
            builtin_matches(builtin, &value),
            "{path:?}: {builtin:?} != {value:?}"
        );
        assert!(seen.insert(entry.template().as_str()));
    }

    assert_eq!(
        seen,
        descriptors()
            .iter()
            .filter(|entry| {
                entry
                    .builtin()
                    .is_some_and(|builtin| builtin != Builtin::Absent)
            })
            .map(|entry| entry.template().as_str())
            .collect()
    );
}

#[test]
fn layer_policy_kind_and_flag_metadata_are_complete() {
    let worktree = descriptors()
        .iter()
        .filter(|entry| entry.layers().contains(&ConfigSource::Worktree))
        .map(|entry| entry.template().as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        worktree,
        [
            "vendor.omp.model",
            "vendor.omp.effort",
            "say.voices",
            "proof.runner",
        ]
    );

    let local = descriptors()
        .iter()
        .filter(|entry| entry.layers().contains(&ConfigSource::Local))
        .map(|entry| entry.template().as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        local,
        [
            "rtk.executable",
            "vendor.omp.model",
            "vendor.omp.effort",
            "listen.window",
            "lock.busy",
            "lock.taken",
            "lock.full",
            "lock.<name>.path",
            "lock.<name>.directory",
            "lock.<name>.shards",
            "serve.pack.rows",
            "say.backend",
            "say.voices",
            "say.words_per_minute",
            "proof.container",
            "release.darwin.identity",
            "release.darwin.notary_profile",
        ]
    );

    let flags = descriptors()
        .iter()
        .filter_map(|entry| entry.flag().map(|flag| (entry.template().as_str(), flag)))
        .collect::<Vec<_>>();
    assert_eq!(flags.len(), 2);
    assert_eq!(flags[0].0, "lane.life");
    assert_eq!(flags[0].1.command(), ["lane", "add"]);
    assert_eq!(flags[0].1.flag(), "--life");
    assert_eq!(flags[1].0, "lane.vendor");
    assert_eq!(flags[1].1.command(), ["lane", "add"]);
    assert_eq!(flags[1].1.flag(), "--vendor");

    for entry in descriptors() {
        let worktree_preference = entry.policy_class() == PolicyClass::WorktreeLocal;
        assert_eq!(
            entry.layers().contains(&ConfigSource::Tracked),
            entry.policy_class() == PolicyClass::Policy,
            "only tracked policy is tracked: {}",
            entry.template().as_str()
        );
        assert!(!entry.layers().contains(&ConfigSource::Builtin));
        assert_eq!(
            entry.policy_class() == PolicyClass::MachineLocal,
            entry.layers().contains(&ConfigSource::Local) && !worktree_preference,
            "{}",
            entry.template().as_str()
        );
        assert_eq!(
            entry.policy_class() == PolicyClass::WorktreeLocal,
            entry.layers().contains(&ConfigSource::Worktree),
            "{}",
            entry.template().as_str()
        );
        assert_eq!(
            entry.layers().contains(&ConfigSource::Flag),
            entry.flag().is_some(),
            "{}",
            entry.template().as_str()
        );

        let compatible = matches!(
            (entry.kind(), entry.builtin()),
            (_, None)
                | (_, Some(Builtin::Absent))
                | (ValueKind::Boolean, Some(Builtin::Boolean(_)))
                | (
                    ValueKind::String | ValueKind::Duration | ValueKind::Answer | ValueKind::Life,
                    Some(Builtin::String(_)),
                )
                | (
                    ValueKind::StringList | ValueKind::DurationList,
                    Some(Builtin::Strings(_)),
                )
                | (
                    ValueKind::UnsignedInteger,
                    Some(Builtin::UnsignedInteger(_)),
                )
                | (ValueKind::Float, Some(Builtin::Float(_)))
        );
        assert!(
            compatible,
            "incompatible builtin for {}",
            entry.template().as_str()
        );
    }
}
