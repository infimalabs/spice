//! Release proof and publication fact bodies.

use serde::{Deserialize, Serialize};

use crate::ident::Digest;

/// The resident recognized a valid evidence bundle bound to commit, tree, and manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofSealed {
    /// Commit the bundle is bound to.
    pub commit: String,
    /// Tree the bundle is bound to.
    pub tree: String,
    /// Manifest the bundle is bound to.
    pub manifest: Digest,
}

/// One published target entry and its detached proof receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedTarget {
    /// Target the entry names.
    pub target: String,
    /// Artifact digest published for the target.
    pub artifact: Digest,
    /// Detached proof receipt the artifact published under.
    pub receipt: Digest,
}

/// The sealed release manifest, notes, and each target artifact digest published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasePublished {
    /// Sealed release manifest digest.
    pub manifest: Digest,
    /// Digest of the exact curated release-note bytes published.
    pub notes: Digest,
    /// Each target entry published under its detached proof receipt.
    pub targets: Vec<PublishedTarget>,
}

/// The resident admitted the spice rack an installed executable links.
///
/// The fact's sequence code is the generation short code a rack URL names:
/// one fact per distinct archive, recognized when the installed image is
/// admitted at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RackAdmitted {
    /// Digest of the linked rack archive bytes.
    pub archive: Digest,
    /// The app generation the rack's `app.html` declares.
    pub generation: Digest,
}
