//! Typed values owned by the stable lock configuration domain.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::LockName;

const BUSY: &str = "lock.busy";
const TAKEN: &str = "lock.taken";
const FULL: &str = "lock.full";
const DEFAULT_BUSY: u8 = 75;
const DEFAULT_TAKEN: u8 = 76;
const DEFAULT_FULL: u8 = 77;

/// Effective advisory-lock configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockConfig {
    exits: ExitStatuses,
    named: BTreeMap<LockName, LockTarget>,
}

impl LockConfig {
    /// Admit exit codes and programmatic named lock pools.
    ///
    /// # Errors
    ///
    /// Refuses zero, overflowing, or repeated exit codes; invalid, reserved,
    /// or duplicate names; and zero or overflowing shard counts.
    pub fn new(
        busy: u64,
        taken: u64,
        full: u64,
        named: impl IntoIterator<Item = (String, u64)>,
    ) -> Result<Self, ConfigError> {
        let exits = ExitStatuses::new(busy, taken, full)?;
        let mut admitted = BTreeMap::new();
        for (name, shards) in named {
            if matches!(name.as_str(), "busy" | "taken" | "full") {
                return Err(ConfigError::ReservedName { name });
            }
            let lock = LockName::new(name.clone())
                .map_err(|_| ConfigError::InvalidName { name: name.clone() })?;
            let target = LockTarget::Pool {
                directory: ConfiguredPath::new(format!("lock.{name}.directory"), name.clone())?,
                shards: ShardCount::new(&name, shards)?,
            };
            if admitted.insert(lock, target).is_some() {
                return Err(ConfigError::DuplicateName { name });
            }
        }
        Ok(Self {
            exits,
            named: admitted,
        })
    }

    /// Process statuses for busy, taken, and full lock outcomes.
    #[must_use]
    pub const fn exits(&self) -> ExitStatuses {
        self.exits
    }

    /// Named lock targets in stable name order.
    pub fn named(&self) -> impl ExactSizeIterator<Item = (&LockName, &LockTarget)> {
        self.named.iter()
    }

    /// Configured target for one named lock or pool.
    #[must_use]
    pub fn target(&self, name: &LockName) -> Option<&LockTarget> {
        self.named.get(name)
    }
}

impl Default for LockConfig {
    fn default() -> Self {
        Self::new(
            u64::from(DEFAULT_BUSY),
            u64::from(DEFAULT_TAKEN),
            u64::from(DEFAULT_FULL),
            std::iter::empty(),
        )
        .expect("the compiled lock configuration is valid")
    }
}

impl Serialize for LockConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3 + self.named.len()))?;
        map.serialize_entry("busy", &self.exits.busy)?;
        map.serialize_entry("taken", &self.exits.taken)?;
        map.serialize_entry("full", &self.exits.full)?;
        for (name, target) in &self.named {
            map.serialize_entry(name.as_str(), target)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for LockConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(LockConfigVisitor)
    }
}

struct LockConfigVisitor;

impl<'de> Visitor<'de> for LockConfigVisitor {
    type Value = LockConfig;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the lock configuration table")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut busy = None;
        let mut taken = None;
        let mut full = None;
        let mut named = Vec::new();

        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "busy" => read_exit(&mut map, &mut busy, BUSY)?,
                "taken" => read_exit(&mut map, &mut taken, TAKEN)?,
                "full" => read_exit(&mut map, &mut full, FULL)?,
                _ => {
                    if named.iter().any(|(name, _)| name == &field) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate configuration field lock.{field}"
                        )));
                    }
                    let target = map.next_value_seed(NamedLockSeed { name: &field })?;
                    named.push((field, target));
                }
            }
        }

        let exits = ExitStatuses::new(
            busy.unwrap_or(u64::from(DEFAULT_BUSY)),
            taken.unwrap_or(u64::from(DEFAULT_TAKEN)),
            full.unwrap_or(u64::from(DEFAULT_FULL)),
        )
        .map_err(serde::de::Error::custom)?;
        let mut admitted = BTreeMap::new();
        for (name, target) in named {
            if matches!(name.as_str(), "busy" | "taken" | "full") {
                return Err(serde::de::Error::custom(ConfigError::ReservedName { name }));
            }
            let lock = LockName::new(name.clone())
                .map_err(|_| serde::de::Error::custom(ConfigError::InvalidName { name }))?;
            admitted.insert(lock, target);
        }
        Ok(LockConfig {
            exits,
            named: admitted,
        })
    }
}

fn read_exit<'de, A: MapAccess<'de>>(
    map: &mut A,
    slot: &mut Option<u64>,
    path: &'static str,
) -> Result<(), A::Error> {
    if slot.is_some() {
        return Err(serde::de::Error::custom(format!(
            "duplicate configuration field {path}"
        )));
    }
    let value = map
        .next_value::<u64>()
        .map_err(|error| serde::de::Error::custom(format!("invalid {path}: {error}")))?;
    *slot = Some(value);
    Ok(())
}

struct NamedLockSeed<'a> {
    name: &'a str,
}

impl<'de> DeserializeSeed<'de> for NamedLockSeed<'_> {
    type Value = LockTarget;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(NamedLockVisitor { name: self.name })
    }
}

struct NamedLockVisitor<'a> {
    name: &'a str,
}

impl<'de> Visitor<'de> for NamedLockVisitor<'_> {
    type Value = LockTarget;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "the lock.{} configuration table", self.name)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let base = format!("lock.{}", self.name);
        let path_path = format!("{base}.path");
        let directory_path = format!("{base}.directory");
        let shards_path = format!("{base}.shards");
        let mut path = None;
        let mut directory = None;
        let mut shards = None;
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "path" => {
                    if path.is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate configuration field {path_path}"
                        )));
                    }
                    let value = map.next_value::<String>().map_err(|error| {
                        serde::de::Error::custom(format!("invalid {path_path}: {error}"))
                    })?;
                    path = Some(
                        ConfiguredPath::new(path_path.clone(), value)
                            .map_err(serde::de::Error::custom)?,
                    );
                }
                "directory" => {
                    if directory.is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate configuration field {directory_path}"
                        )));
                    }
                    let value = map.next_value::<String>().map_err(|error| {
                        serde::de::Error::custom(format!("invalid {directory_path}: {error}"))
                    })?;
                    directory = Some(
                        ConfiguredPath::new(directory_path.clone(), value)
                            .map_err(serde::de::Error::custom)?,
                    );
                }
                "shards" => {
                    if shards.is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate configuration field {shards_path}"
                        )));
                    }
                    let value = map.next_value::<u64>().map_err(|error| {
                        serde::de::Error::custom(format!("invalid {shards_path}: {error}"))
                    })?;
                    shards =
                        Some(ShardCount::new(self.name, value).map_err(serde::de::Error::custom)?);
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                    return Err(serde::de::Error::custom(format!(
                        "unknown configuration field {base}.{field}"
                    )));
                }
            }
        }
        match (path, directory, shards) {
            (Some(path), None, None) => Ok(LockTarget::File { path }),
            (Some(_), Some(_), _) => Err(serde::de::Error::custom(format!(
                "{base} must declare exactly one of {path_path} or {directory_path}"
            ))),
            (Some(_), None, Some(_)) => Err(serde::de::Error::custom(format!(
                "{shards_path} is only valid with {directory_path}"
            ))),
            (None, Some(directory), shards) => Ok(LockTarget::Pool {
                directory,
                shards: shards.unwrap_or(ShardCount(1)),
            }),
            (None, None, _) => Err(serde::de::Error::custom(format!(
                "{base} must declare exactly one of {path_path} or {directory_path}"
            ))),
        }
    }
}

/// One explicit configured lock location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredPath(PathBuf);

impl ConfiguredPath {
    fn new(path: String, value: String) -> Result<Self, ConfigError> {
        let candidate = Path::new(&value);
        let mut normal = 0usize;
        for component in candidate.components() {
            match component {
                Component::Normal(_) => normal += 1,
                Component::RootDir | Component::Prefix(_) => {}
                Component::CurDir | Component::ParentDir => {
                    return Err(ConfigError::InvalidPath {
                        path,
                        detail: "must not contain current-directory or parent-directory components",
                    });
                }
            }
        }
        if value.trim().is_empty() || value.contains('\0') || normal == 0 {
            return Err(ConfigError::InvalidPath {
                path,
                detail: "must be a nonempty file-system path",
            });
        }
        Ok(Self(candidate.to_path_buf()))
    }

    /// Exact configured path before repository-relative resolution.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl Serialize for ConfiguredPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string_lossy())
    }
}

/// Either one lock file or a directory containing a fixed shard pool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockTarget {
    File {
        path: ConfiguredPath,
    },
    Pool {
        directory: ConfiguredPath,
        shards: ShardCount,
    },
}

impl LockTarget {
    /// Configured file or directory path.
    #[must_use]
    pub const fn location(&self) -> &ConfiguredPath {
        match self {
            Self::File { path } => path,
            Self::Pool { directory, .. } => directory,
        }
    }

    /// True for a single-file lock rather than a shard pool.
    #[must_use]
    pub const fn is_file(&self) -> bool {
        matches!(self, Self::File { .. })
    }

    /// Number of acquisition slots represented by this target.
    #[must_use]
    pub const fn shards(&self) -> ShardCount {
        match self {
            Self::File { .. } => ShardCount(1),
            Self::Pool { shards, .. } => *shards,
        }
    }
}

impl Serialize for LockTarget {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::File { path } => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("path", path)?;
                map.end()
            }
            Self::Pool { directory, shards } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("directory", directory)?;
                map.serialize_entry("shards", shards)?;
                map.end()
            }
        }
    }
}

/// Three distinct non-success process statuses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitStatuses {
    busy: ExitCode,
    taken: ExitCode,
    full: ExitCode,
}

impl ExitStatuses {
    fn new(busy: u64, taken: u64, full: u64) -> Result<Self, ConfigError> {
        let statuses = Self {
            busy: ExitCode::new(BUSY, busy)?,
            taken: ExitCode::new(TAKEN, taken)?,
            full: ExitCode::new(FULL, full)?,
        };
        for (left_path, left, right_path, right) in [
            (BUSY, statuses.busy, TAKEN, statuses.taken),
            (BUSY, statuses.busy, FULL, statuses.full),
            (TAKEN, statuses.taken, FULL, statuses.full),
        ] {
            if left == right {
                return Err(ConfigError::DuplicateExit {
                    first: left_path,
                    second: right_path,
                    code: left,
                });
            }
        }
        Ok(statuses)
    }

    /// Status emitted when immediate acquisition finds contention.
    #[must_use]
    pub const fn busy(self) -> ExitCode {
        self.busy
    }

    /// Status emitted when an explicitly selected shard is already held.
    #[must_use]
    pub const fn taken(self) -> ExitCode {
        self.taken
    }

    /// Status emitted when a named pool has no shard available.
    #[must_use]
    pub const fn full(self) -> ExitCode {
        self.full
    }
}

/// One valid non-success process status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ExitCode(u8);

impl ExitCode {
    fn new(path: &'static str, value: u64) -> Result<Self, ConfigError> {
        if value == 0 {
            return Err(ConfigError::ZeroExit { path });
        }
        let code = u8::try_from(value).map_err(|_| ConfigError::ExitOverflow { path, value })?;
        Ok(Self(code))
    }

    /// Runtime process code.
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0 as i32
    }
}

/// Positive portable capacity of one named lock pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ShardCount(u32);

impl ShardCount {
    fn new(name: &str, value: u64) -> Result<Self, ConfigError> {
        if value == 0 {
            return Err(ConfigError::ZeroShards {
                name: name.to_owned(),
            });
        }
        let count = u32::try_from(value).map_err(|_| ConfigError::ShardsOverflow {
            name: name.to_owned(),
            value,
        })?;
        Ok(Self(count))
    }

    /// Portable configured shard count.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Why an effective lock value was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    /// A process status attempted to spell success.
    ZeroExit { path: &'static str },
    /// A process status exceeded the platform status byte.
    ExitOverflow { path: &'static str, value: u64 },
    /// Two outcome classes shared one process status.
    DuplicateExit {
        first: &'static str,
        second: &'static str,
        code: ExitCode,
    },
    /// A dynamic lock name failed the owning vocabulary.
    InvalidName { name: String },
    /// A dynamic lock name occupied a scalar key.
    ReservedName { name: String },
    /// A dynamic lock name repeated.
    DuplicateName { name: String },
    /// A named pool had no usable shard.
    ZeroShards { name: String },
    /// A named pool exceeded the portable count range.
    ShardsOverflow { name: String, value: u64 },
    /// A configured file or directory path was empty or unsafe.
    InvalidPath { path: String, detail: &'static str },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroExit { path } => write!(formatter, "{path} must be a nonzero process code"),
            Self::ExitOverflow { path, value } => {
                write!(formatter, "{path} exceeds process code 255: {value}")
            }
            Self::DuplicateExit {
                first,
                second,
                code,
            } => write!(
                formatter,
                "{first} and {second} must differ; both are {}",
                code.get()
            ),
            Self::InvalidName { name } => write!(
                formatter,
                "lock.{name} does not satisfy the lock name vocabulary"
            ),
            Self::ReservedName { name } => {
                write!(
                    formatter,
                    "lock.{name} is reserved for a scalar exit status"
                )
            }
            Self::DuplicateName { name } => {
                write!(formatter, "lock.{name} is declared more than once")
            }
            Self::ZeroShards { name } => write!(formatter, "lock.{name}.shards must be positive"),
            Self::ShardsOverflow { name, value } => write!(
                formatter,
                "lock.{name}.shards exceeds the maximum {}: {value}",
                u32::MAX
            ),
            Self::InvalidPath { path, detail } => write!(formatter, "{path} {detail}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_defaults_and_dynamic_rows_round_trip_in_stable_order() {
        let builtin = LockConfig::default();
        assert_eq!(builtin.exits().busy().get(), 75);
        assert_eq!(builtin.exits().taken().get(), 76);
        assert_eq!(builtin.exits().full().get(), 77);
        assert_eq!(builtin.named().len(), 0);
        assert_eq!(
            serde_json::from_str::<LockConfig>("{}").expect("empty lock table"),
            builtin
        );

        let configured: LockConfig = serde_json::from_str(concat!(
            r#"{"busy":70,"taken":71,"full":72,"zeta":{"directory":"locks/zeta","shards":2},"#,
            r#""alpha":{"directory":"locks/alpha","shards":3}}"#,
        ))
        .expect("decode lock config");
        let alpha = LockName::new("alpha").expect("lock name");
        assert_eq!(
            configured
                .target(&alpha)
                .map(LockTarget::shards)
                .map(ShardCount::get),
            Some(3)
        );
        assert_eq!(
            configured
                .named()
                .map(|(name, target)| (name.as_str(), target.shards().get()))
                .collect::<Vec<_>>(),
            [("alpha", 3), ("zeta", 2)]
        );
        let bytes = serde_json::to_vec(&configured).expect("encode lock config");
        assert_eq!(
            bytes,
            concat!(
                r#"{"busy":70,"taken":71,"full":72,"#,
                r#""alpha":{"directory":"locks/alpha","shards":3},"#,
                r#""zeta":{"directory":"locks/zeta","shards":2}}"#,
            )
            .as_bytes()
        );
        assert_eq!(
            serde_json::from_slice::<LockConfig>(&bytes).expect("round trip"),
            configured
        );
        let document = toml::to_string(&configured).expect("encode TOML");
        assert_eq!(
            toml::from_str::<LockConfig>(&document).expect("round-trip TOML"),
            configured
        );
    }

    #[test]
    fn every_scalar_and_dynamic_refusal_names_its_full_path() {
        for (document, path) in [
            (r#"{"busy":0}"#, BUSY),
            (r#"{"taken":256}"#, TAKEN),
            (r#"{"taken":"76"}"#, TAKEN),
            (r#"{"full":75}"#, FULL),
            (r#"{"worker":1}"#, "lock.worker"),
            (r#"{"worker":{}}"#, "lock.worker"),
            (
                r#"{"worker":{"directory":"locks/worker","shards":0}}"#,
                "lock.worker.shards",
            ),
            (
                r#"{"worker":{"directory":"locks/worker","shards":4294967296}}"#,
                "lock.worker.shards",
            ),
            (r#"{"worker":{"other":1}}"#, "lock.worker.other"),
            (r#"{"":{"path":"locks/worker.lock"}}"#, "lock."),
            (r#"{"worker":{"path":""}}"#, "lock.worker.path"),
            (
                r#"{"worker":{"path":"../outside.lock"}}"#,
                "lock.worker.path",
            ),
            (
                r#"{"worker":{"path":"locks/worker.lock","directory":"locks/worker"}}"#,
                "lock.worker",
            ),
            (
                r#"{"worker":{"path":"locks/worker.lock","shards":1}}"#,
                "lock.worker.shards",
            ),
        ] {
            let error =
                serde_json::from_str::<LockConfig>(document).expect_err("config must refuse");
            assert!(error.to_string().contains(path), "{error}");
        }
    }

    #[test]
    fn exit_classes_are_pairwise_distinct() {
        for document in [
            r#"{"busy":75,"taken":75}"#,
            r#"{"busy":77,"full":77}"#,
            r#"{"taken":77,"full":77}"#,
        ] {
            assert!(serde_json::from_str::<LockConfig>(document).is_err());
        }
    }

    #[test]
    fn duplicate_scalar_and_dynamic_paths_refuse_by_name() {
        for (document, path) in [
            (r#"{"busy":70,"busy":71}"#, BUSY),
            (
                r#"{"worker":{"directory":"locks/one"},"worker":{"directory":"locks/two"}}"#,
                "lock.worker",
            ),
            (
                r#"{"worker":{"directory":"locks/worker","shards":1,"shards":2}}"#,
                "lock.worker.shards",
            ),
            (
                r#"{"worker":{"path":"one.lock","path":"two.lock"}}"#,
                "lock.worker.path",
            ),
        ] {
            let error =
                serde_json::from_str::<LockConfig>(document).expect_err("duplicate must refuse");
            assert!(error.to_string().contains(path), "{error}");
        }
    }
}
