//! Typed repository command mounts admitted by the configuration root.

use std::collections::BTreeMap;

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize};

/// Repository-owned command paths and their exact child argv.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CommandMounts(BTreeMap<String, CommandArgv>);

impl CommandMounts {
    /// Iterate mounts in deterministic command-path order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &[String])> {
        self.0
            .iter()
            .map(|(name, arguments)| (name.as_str(), arguments.0.as_slice()))
    }
}

impl<'de> Deserialize<'de> for CommandMounts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mounts = BTreeMap::<String, CommandArgv>::deserialize(deserializer)?;
        for name in mounts.keys() {
            if !valid_path(name) {
                return Err(D::Error::custom(format!(
                    concat!(
                        "[commands] entry {:?} must be dot-separated segments ",
                        "matching ^[a-z][a-z0-9-]*$"
                    ),
                    name
                )));
            }
        }
        Ok(Self(mounts))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
struct CommandArgv(Vec<String>);

impl<'de> Deserialize<'de> for CommandArgv {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let arguments = Vec::<String>::deserialize(deserializer)?;
        if arguments.is_empty() {
            return Err(D::Error::custom("[commands] argv must contain a program"));
        }
        if arguments[0].is_empty() {
            return Err(D::Error::custom("[commands] argv program must be nonempty"));
        }
        Ok(Self(arguments))
    }
}

fn valid_path(path: &str) -> bool {
    !path.is_empty() && path.split('.').all(valid_segment)
}

fn valid_segment(segment: &str) -> bool {
    let mut bytes = segment.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_argv_are_current_and_exact() {
        let mounts: CommandMounts = toml::from_str(
            r#"
            release = ["tool", "release", ""]
            "report.inspect" = ["tool", "report", "inspect"]
            "#,
        )
        .expect("valid mount table");

        assert_eq!(
            mounts
                .iter()
                .map(|(name, argv)| { (name, argv.iter().map(String::as_str).collect::<Vec<_>>()) })
                .collect::<Vec<_>>(),
            [
                ("release", vec!["tool", "release", ""]),
                ("report.inspect", vec!["tool", "report", "inspect"]),
            ]
        );
    }

    #[test]
    fn malformed_paths_and_programs_refuse() {
        for (document, expected) in [
            ("Bad = ['tool']", "dot-separated segments"),
            ("'two..parts' = ['tool']", "dot-separated segments"),
            ("empty = []", "must contain a program"),
            ("empty = ['']", "program must be nonempty"),
            ("string = 'tool --flag'", "invalid type"),
        ] {
            let error = toml::from_str::<CommandMounts>(document)
                .expect_err("malformed mount table must refuse")
                .to_string();
            assert!(error.contains(expected), "{document:?} refused as {error}");
        }
    }
}
