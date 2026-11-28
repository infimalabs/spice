//! Canonical identity of the external source set a proof image vendors.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const DOMAIN: &[u8] = b"spice.proof.vendor-set.v1\0";
const MAX_LOCK_BYTES: u64 = 16 * 1024 * 1024;
/// OCI image label carrying the vendored source-set digest.
pub const VENDOR_SET_LABEL: &str = "org.spice.vendor-set";
/// OCI image label carrying the prepared build key for a hosted proof image.
pub const PROOF_IMAGE_BUILD_KEY_LABEL: &str = "org.spice.build-key";
/// Local OCI repository whose tag is the vendor-set digest.
pub const PROOF_IMAGE_REPOSITORY: &str = "spice-proof-image";

/// Digest of every external package source Cargo vendors for one lockfile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VendorSet(String);

impl VendorSet {
    /// Read and fingerprint the external package set in `Cargo.lock`.
    ///
    /// Package order, dependency edges, formatting, and path packages do not
    /// affect the result. Registry and Git package identity does.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, VendorSetError> {
        let path = path.as_ref();
        let metadata = fs::metadata(path).map_err(|source| VendorSetError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.len() > MAX_LOCK_BYTES {
            return Err(VendorSetError::TooLarge {
                path: path.to_path_buf(),
                bytes: metadata.len(),
                limit: MAX_LOCK_BYTES,
            });
        }
        let text = fs::read_to_string(path).map_err(|source| VendorSetError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(path, &text)
    }

    #[doc(hidden)]
    pub fn parse(path: &Path, text: &str) -> Result<Self, VendorSetError> {
        let lock: Lock = toml::from_str(text).map_err(|source| VendorSetError::Decode {
            path: path.to_path_buf(),
            source,
        })?;
        let mut packages = lock
            .package
            .into_iter()
            .filter_map(|package| {
                package.source.map(|source| Package {
                    name: package.name,
                    version: package.version,
                    source,
                    checksum: package.checksum,
                })
            })
            .collect::<Vec<_>>();
        packages.sort();
        packages.dedup();

        let mut digest = Sha256::new();
        digest.update(DOMAIN);
        for package in packages {
            frame(&mut digest, package.name.as_bytes());
            frame(&mut digest, package.version.as_bytes());
            frame(&mut digest, package.source.as_bytes());
            match package.checksum {
                Some(checksum) => {
                    digest.update([1]);
                    frame(&mut digest, checksum.as_bytes());
                }
                None => digest.update([0]),
            }
        }
        Ok(Self(format!("sha256:{:x}", digest.finalize())))
    }

    /// Canonical OCI-label spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Deterministic local image tag for this exact external package set.
    #[must_use]
    pub fn image_tag(&self) -> String {
        format!(
            "{PROOF_IMAGE_REPOSITORY}:{}",
            self.0
                .strip_prefix("sha256:")
                .expect("vendor-set identities always use SHA-256")
        )
    }
}

impl fmt::Display for VendorSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn frame(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

#[derive(Deserialize)]
struct Lock {
    #[serde(default)]
    package: Vec<LockPackage>,
}

#[derive(Deserialize)]
struct LockPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Package {
    name: String,
    version: String,
    source: String,
    checksum: Option<String>,
}

/// Why a lockfile could not name its vendored source set.
#[derive(Debug)]
pub enum VendorSetError {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    TooLarge {
        path: PathBuf,
        bytes: u64,
        limit: u64,
    },
    Decode {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl fmt::Display for VendorSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::TooLarge { path, bytes, limit } => write!(
                formatter,
                "{} is {bytes} bytes, exceeding the {limit}-byte lockfile limit",
                path.display()
            ),
            Self::Decode { path, source } => {
                write!(formatter, "cannot decode {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for VendorSetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Decode { source, .. } => Some(source),
            Self::TooLarge { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: &str = r#"
version = 4

[[package]]
name = "workspace"
version = "0.0.0"
dependencies = ["external"]

[[package]]
name = "external"
version = "1.2.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "1111"
dependencies = ["other"]

[[package]]
name = "git-package"
version = "2.0.0"
source = "git+https://example.invalid/repository#0123456789abcdef"
"#;

    const REORDERED: &str = r#"
version=4
[[package]]
name="git-package"
source="git+https://example.invalid/repository#0123456789abcdef"
version="2.0.0"

[[package]]
name="external"
checksum="1111"
source="registry+https://github.com/rust-lang/crates.io-index"
version="1.2.3"
dependencies=["changed-edge"]

[[package]]
name="another-path-package"
version="9.9.9"
"#;

    fn parsed(text: &str) -> VendorSet {
        VendorSet::parse(Path::new("Cargo.lock"), text).expect("fixture lockfile parses")
    }

    #[test]
    fn ordering_formatting_edges_and_path_packages_do_not_change_the_vendor_set() {
        assert_eq!(parsed(FIRST), parsed(REORDERED));
    }

    #[test]
    fn image_tag_is_derived_only_from_the_vendor_set() {
        let vendor_set = parsed(FIRST);
        assert_eq!(
            vendor_set.image_tag(),
            format!(
                "{PROOF_IMAGE_REPOSITORY}:{}",
                vendor_set.as_str().strip_prefix("sha256:").unwrap()
            )
        );
        assert_eq!(vendor_set.image_tag(), parsed(REORDERED).image_tag());
    }

    #[test]
    fn external_identity_and_checksum_change_the_vendor_set() {
        let expected = parsed(FIRST);
        assert_ne!(expected, parsed(&FIRST.replace("1.2.3", "1.2.4")));
        assert_ne!(
            expected,
            parsed(&FIRST.replace("checksum = \"1111\"", "checksum = \"2222\""))
        );
        assert_ne!(
            expected,
            parsed(&FIRST.replace("crates.io-index", "mirror.invalid/index"))
        );
    }
}
