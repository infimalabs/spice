//! The process environment, captured once at the binary rim and passed inward.
//!
//! Nothing in the workspace sets or removes process variables, so one snapshot
//! taken when the process starts reads exactly what a later direct read would.

use std::ffi::{OsStr, OsString};

pub mod session;

/// The process environment as one binary captured it at startup.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Ambient {
    variables: Vec<(OsString, OsString)>,
}

impl Ambient {
    /// An environment holding exactly `variables`.
    pub fn new<I, K, V>(variables: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        Self {
            variables: variables
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        }
    }

    /// The value of `name`, matched as the platform matches variable names:
    /// exactly on Unix, ignoring ASCII case on Windows.
    #[must_use]
    pub fn get(&self, name: impl AsRef<OsStr>) -> Option<&OsStr> {
        let name = name.as_ref();
        self.variables
            .iter()
            .find(|(candidate, _)| same_name(candidate, name))
            .map(|(_, value)| value.as_os_str())
    }

    /// Every variable, in capture order.
    #[must_use]
    pub fn variables(&self) -> &[(OsString, OsString)] {
        &self.variables
    }

    /// A child-process environment carrying every captured variable.
    ///
    /// # Errors
    ///
    /// Returns the [`sill::SpecError`] for an empty or repeated name.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn environment(&self) -> Result<sill::Environment, sill::SpecError> {
        sill::Environment::new(self.variables.iter().cloned())
    }
}

#[cfg(windows)]
fn same_name(candidate: &OsStr, name: &OsStr) -> bool {
    candidate.eq_ignore_ascii_case(name)
}

#[cfg(not(windows))]
fn same_name(candidate: &OsStr, name: &OsStr) -> bool {
    candidate == name
}

/// Capture this process's environment.
///
/// A binary calls this once from `main` and threads the result to every
/// reader; in-process tests call it to run tools the way the binary would.
#[must_use]
#[expect(
    clippy::disallowed_methods,
    reason = "the spice binary's single capture of its process environment"
)]
pub fn capture() -> Ambient {
    Ambient::new(std::env::vars_os())
}
