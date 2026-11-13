//! Operator-owned names for the Developer ID signature a Darwin release carries.
//!
//! Configuration holds only names macOS resolves: a keychain signing identity
//! and a `notarytool` keychain profile. No certificate, private key, or
//! notarization credential is ever admitted here.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

/// The common-name prefix of every Developer ID Application certificate.
const DEVELOPER_ID_PREFIX: &str = "Developer ID Application: ";

/// One Developer ID Application signing identity name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DeveloperIdentity(String);

impl DeveloperIdentity {
    /// Admit one identity spelled as `security find-identity` prints it.
    ///
    /// # Errors
    ///
    /// Refuses a name that is not a Developer ID Application identity.
    pub fn new(name: impl Into<String>) -> Result<Self, DarwinSigningError> {
        let name = name.into();
        match name.strip_prefix(DEVELOPER_ID_PREFIX) {
            Some(holder) if !holder.trim().is_empty() => Ok(Self(name)),
            _ => Err(DarwinSigningError::Identity(name)),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DeveloperIdentity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// One `notarytool` keychain profile name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NotaryProfile(String);

impl NotaryProfile {
    /// Admit one profile name stored by `xcrun notarytool store-credentials`.
    ///
    /// # Errors
    ///
    /// Refuses an empty or whitespace-only name.
    pub fn new(name: impl Into<String>) -> Result<Self, DarwinSigningError> {
        let name = name.into();
        if name.trim().is_empty() {
            Err(DarwinSigningError::NotaryProfile)
        } else {
            Ok(Self(name))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for NotaryProfile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// A complete Developer ID signing request: who signs and who notarizes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DarwinSigning {
    identity: DeveloperIdentity,
    notary_profile: NotaryProfile,
}

impl DarwinSigning {
    #[must_use]
    pub const fn new(identity: DeveloperIdentity, notary_profile: NotaryProfile) -> Self {
        Self {
            identity,
            notary_profile,
        }
    }

    #[must_use]
    pub const fn identity(&self) -> &DeveloperIdentity {
        &self.identity
    }

    #[must_use]
    pub const fn notary_profile(&self) -> &NotaryProfile {
        &self.notary_profile
    }
}

/// The `[release]` configuration domain.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReleaseConfig {
    darwin: DarwinSigningConfig,
}

impl ReleaseConfig {
    /// The configured Darwin signing pair.
    ///
    /// Each name is set by its own configuration write, so either may be
    /// present alone; staging admits only the pair or neither.
    ///
    /// # Errors
    ///
    /// Names the missing key when exactly one of the pair is configured.
    pub fn darwin_signing(&self) -> Result<Option<DarwinSigning>, DarwinSigningError> {
        match (&self.darwin.identity, &self.darwin.notary_profile) {
            (None, None) => Ok(None),
            (Some(identity), Some(profile)) => {
                Ok(Some(DarwinSigning::new(identity.clone(), profile.clone())))
            }
            (Some(_), None) => Err(DarwinSigningError::Unpaired(
                "release.darwin.notary_profile",
            )),
            (None, Some(_)) => Err(DarwinSigningError::Unpaired("release.darwin.identity")),
        }
    }

}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
struct DarwinSigningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    identity: Option<DeveloperIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notary_profile: Option<NotaryProfile>,
}


/// Why a Darwin signing name or pair was refused.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DarwinSigningError {
    #[error(
        "{0:?} is not a Developer ID Application identity; name it as `security find-identity -v -p codesigning` prints it, beginning {DEVELOPER_ID_PREFIX:?}"
    )]
    Identity(String),
    #[error("a notarytool keychain profile name cannot be empty")]
    NotaryProfile,
    #[error("Darwin signing names its identity and notary profile together; {0} is not configured")]
    Unpaired(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(document: &str) -> Result<ReleaseConfig, toml::de::Error> {
        toml::from_str(document)
    }

    #[test]
    fn signing_admits_the_configured_pair_or_neither_and_names_a_missing_half() {
        assert_eq!(config("").expect("empty").darwin_signing(), Ok(None));

        let paired = config(
            "[darwin]\nidentity = \"Developer ID Application: Holder (TEAM123456)\"\nnotary_profile = \"spice\"\n",
        )
        .expect("pair")
        .darwin_signing()
        .expect("pair admits")
        .expect("pair is configured");
        assert_eq!(
            paired.identity().as_str(),
            "Developer ID Application: Holder (TEAM123456)"
        );
        assert_eq!(paired.notary_profile().as_str(), "spice");

        assert_eq!(
            config("[darwin]\nidentity = \"Developer ID Application: Holder (TEAM123456)\"\n")
                .expect("identity alone parses")
                .darwin_signing(),
            Err(DarwinSigningError::Unpaired(
                "release.darwin.notary_profile"
            ))
        );
        assert_eq!(
            config("[darwin]\nnotary_profile = \"spice\"\n")
                .expect("profile alone parses")
                .darwin_signing(),
            Err(DarwinSigningError::Unpaired("release.darwin.identity"))
        );
    }


    #[test]
    fn only_a_named_developer_id_application_identity_is_admitted() {
        for refused in [
            "Apple Development: Holder (TEAM123456)",
            "Developer ID Installer: Holder (TEAM123456)",
            DEVELOPER_ID_PREFIX,
            "Developer ID Application:  ",
            "",
        ] {
            assert_eq!(
                DeveloperIdentity::new(refused),
                Err(DarwinSigningError::Identity(refused.to_owned())),
                "{refused:?}"
            );
            assert!(
                config(&format!("[darwin]\nidentity = {refused:?}\n")).is_err(),
                "{refused:?}"
            );
        }
        assert_eq!(
            NotaryProfile::new(" "),
            Err(DarwinSigningError::NotaryProfile)
        );
        assert!(config("[darwin]\nnotary_profile = \"\"\n").is_err());
    }
}
