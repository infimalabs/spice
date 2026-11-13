//! Small terminal answers shared by command bindings and residents.

use serde::{Deserialize, Serialize};

use crate::{Digest, Name};

/// The answer a taste verdict returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TasteAnswer {
    /// Bag the verdict landed in.
    pub name: Name,
    /// Whether the judge confirmed the hit.
    pub confirmed: bool,
}

/// The answer a recognition returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecognitionAnswer {
    /// Observation key that was recognized.
    pub key: Digest,
    /// False when the key had already been recognized.
    pub fresh: bool,
}
