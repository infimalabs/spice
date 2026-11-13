//! Exact build-tool pin admission.

use std::fmt;

use wire::manifest::{BuildPins, ProofImage};

/// One pin in the fixed pre-build comparison order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinName {
    Rust,
    WasmTarget,
    Trunk,
    ProofContainer,
}

impl PinName {
    const ALL: [Self; 4] = [
        Self::Rust,
        Self::WasmTarget,
        Self::Trunk,
        Self::ProofContainer,
    ];

    fn value(self, pins: &BuildPins) -> &str {
        match self {
            Self::Rust => &pins.rust,
            Self::WasmTarget => &pins.wasm_target,
            Self::Trunk => &pins.trunk,
            Self::ProofContainer => &pins.proof_container,
        }
    }
}

impl fmt::Display for PinName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Rust => "rust",
            Self::WasmTarget => "wasm-target",
            Self::Trunk => "trunk",
            Self::ProofContainer => "proof-container",
        })
    }
}

/// Validated exact pins required by one pipeline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequiredPins(BuildPins);

impl RequiredPins {
    /// Admit a complete pin set.
    ///
    /// # Errors
    ///
    /// Returns [`PinError::Empty`] when any identity is blank and
    /// [`PinError::ContainerNotDigestPinned`] when the proof image is not an
    /// canonical lower-case SHA-256 digest reference.
    pub fn new(pins: BuildPins) -> Result<Self, PinError> {
        for name in PinName::ALL {
            if name.value(&pins).trim().is_empty() {
                return Err(PinError::Empty { name });
            }
        }
        if !ProofImage::admits(&pins.proof_container) {
            return Err(PinError::ContainerNotDigestPinned {
                observed: pins.proof_container,
            });
        }
        Ok(Self(pins))
    }

    /// Compare observed identities before any build work starts.
    ///
    /// # Errors
    ///
    /// Returns the first mismatch in the fixed comparison order.
    pub fn verify(&self, observed: &BuildPins) -> Result<(), PinError> {
        for name in PinName::ALL {
            let expected = name.value(&self.0);
            let actual = name.value(observed);
            if expected != actual {
                return Err(PinError::Mismatch {
                    name,
                    expected: expected.to_owned(),
                    observed: actual.to_owned(),
                });
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn get(&self) -> &BuildPins {
        &self.0
    }
}

/// A refusal raised before build execution.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PinError {
    #[error("required {name} pin is empty")]
    Empty { name: PinName },
    #[error(
        "proof container must use a canonical lower-case SHA-256 digest reference: {observed:?}"
    )]
    ContainerNotDigestPinned { observed: String },
    #[error("{name} pin mismatch: expected {expected:?}, observed {observed:?}")]
    Mismatch {
        name: PinName,
        expected: String,
        observed: String,
    },
}
