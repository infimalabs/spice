//! Exact filesystem inputs for one release target's proof.
//!
//! Proof commands take no path operands. Their only filesystem choices are
//! therefore derived from repository roots the executable already resolved,
//! the selected release target, and the admitted target payload. This module
//! joins those typed values directly and reads only the resulting leaf: it
//! performs no discovery and treats every unusable input as its own failure.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use wire::manifest::{ReleaseTarget, TargetPayload};

/// The exact payload and receipt namespace for one selected target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    target: ReleaseTarget,
    payload: PathBuf,
    proof_root: PathBuf,
}

impl Layout {
    /// Derives one layout from caller-resolved worktree and proof roots.
    #[must_use]
    pub fn new(
        worktree_root: impl Into<PathBuf>,
        proof_root: impl Into<PathBuf>,
        target: ReleaseTarget,
    ) -> Self {
        let payload = worktree_root
            .into()
            .join("dist")
            .join("stage")
            .join("targets")
            .join(target.as_str())
            .join("payload.json");
        let proof_root = proof_root.into();
        Self {
            target,
            payload,
            proof_root,
        }
    }

    /// Release target this layout was selected for.
    #[must_use]
    pub const fn target(&self) -> ReleaseTarget {
        self.target
    }

    /// The sole target-payload path.
    #[must_use]
    pub fn payload_path(&self) -> &Path {
        &self.payload
    }

    /// Reads the sole target-payload path.
    ///
    /// # Errors
    ///
    /// Returns the exact input state and path when the payload cannot be read
    /// as one regular, non-symlink file.
    pub fn read_payload(&self) -> Result<Vec<u8>, InputError> {
        read_exact(&self.payload, Input::Payload)
    }

    /// Derives the sole detached-receipt input from an admitted payload.
    ///
    /// # Errors
    ///
    /// Refuses a payload for another target before its source digest can name
    /// a receipt directory.
    pub fn receipt_for(&self, payload: &TargetPayload) -> Result<ReceiptInput, LayoutError> {
        if payload.target != self.target {
            return Err(LayoutError::Target {
                selected: self.target,
                payload: payload.target,
            });
        }
        Ok(ReceiptInput {
            path: self
                .proof_root
                .join(payload.source.as_str())
                .join(self.target.as_str())
                .join("receipt.json"),
        })
    }
}

/// The one detached-receipt leaf derived for an admitted payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiptInput {
    path: PathBuf,
}

impl ReceiptInput {
    /// The sole detached-receipt path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the sole detached-receipt path.
    ///
    /// # Errors
    ///
    /// Returns the exact input state and path when the receipt cannot be read
    /// as one regular, non-symlink file.
    pub fn read(&self) -> Result<Vec<u8>, InputError> {
        read_exact(&self.path, Input::Receipt)
    }
}

/// Which exact proof input failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Input {
    Payload,
    Receipt,
}

impl fmt::Display for Input {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Payload => "target payload",
            Self::Receipt => "proof receipt",
        })
    }
}

/// Why the layout could not be completed from admitted identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutError {
    Target {
        selected: ReleaseTarget,
        payload: ReleaseTarget,
    },
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Target { selected, payload } => write!(
                formatter,
                "selected proof target {selected} does not match payload target {payload}"
            ),
        }
    }
}

impl Error for LayoutError {}

/// Why one exact proof input could not be read.
#[derive(Debug)]
pub enum InputError {
    Missing {
        input: Input,
        path: PathBuf,
    },
    Symlink {
        input: Input,
        path: PathBuf,
    },
    NotRegular {
        input: Input,
        path: PathBuf,
    },
    Unreadable {
        input: Input,
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for InputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { input, path } => {
                write!(formatter, "{input} is missing: {}", path.display())
            }
            Self::Symlink { input, path } => {
                write!(formatter, "{input} is a symlink: {}", path.display())
            }
            Self::NotRegular { input, path } => write!(
                formatter,
                "{input} is not a regular file: {}",
                path.display()
            ),
            Self::Unreadable {
                input,
                path,
                source,
            } => write!(
                formatter,
                "{input} is unreadable at {}: {source}",
                path.display()
            ),
        }
    }
}

impl Error for InputError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Unreadable { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn read_exact(path: &Path, input: Input) -> Result<Vec<u8>, InputError> {
    read_exact_with(path, input, |path| fs::read(path))
}

fn read_exact_with(
    path: &Path,
    input: Input,
    read: impl FnOnce(&Path) -> io::Result<Vec<u8>>,
) -> Result<Vec<u8>, InputError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(input, path, source))?;
    if metadata.file_type().is_symlink() {
        return Err(InputError::Symlink {
            input,
            path: path.to_path_buf(),
        });
    }
    if !metadata.is_file() {
        return Err(InputError::NotRegular {
            input,
            path: path.to_path_buf(),
        });
    }
    read(path).map_err(|source| io_error(input, path, source))
}

fn io_error(input: Input, path: &Path, source: io::Error) -> InputError {
    if source.kind() == io::ErrorKind::NotFound {
        InputError::Missing {
            input,
            path: path.to_path_buf(),
        }
    } else {
        InputError::Unreadable {
            input,
            path: path.to_path_buf(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use wire::manifest::{BuildPins, InventoryEntry};
    use wire::{Digest, Domain};

    use super::*;

    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn all_targets_derive_only_their_exact_payload_and_receipt_paths() {
        let scratch = Scratch::new();
        let worktree = scratch.join("worktree");
        let common = scratch.join("common");
        let state = common.join(".spice");

        for target in [
            ReleaseTarget::Aarch64AppleDarwin,
            ReleaseTarget::X86_64UnknownLinuxMusl,
            ReleaseTarget::X86_64PcWindowsMsvc,
        ] {
            let layout = Layout::new(&worktree, state.join("proof"), target);
            let payload = payload(target, target.as_str());

            assert_eq!(layout.target(), target);
            assert_eq!(
                layout.payload_path(),
                worktree
                    .join("dist/stage/targets")
                    .join(target.as_str())
                    .join("payload.json")
            );
            assert_eq!(
                layout
                    .receipt_for(&payload)
                    .expect("matching payload")
                    .path(),
                common
                    .join(".spice/proof")
                    .join(payload.source.as_str())
                    .join(target.as_str())
                    .join("receipt.json")
            );
        }
    }

    #[test]
    fn a_payload_for_another_target_cannot_redirect_receipt_selection() {
        let scratch = Scratch::new();
        let layout = Layout::new(
            scratch.join("worktree"),
            scratch.join("common/.spice/proof"),
            ReleaseTarget::Aarch64AppleDarwin,
        );
        let payload = payload(ReleaseTarget::X86_64UnknownLinuxMusl, "other target");

        assert_eq!(
            layout.receipt_for(&payload),
            Err(LayoutError::Target {
                selected: ReleaseTarget::Aarch64AppleDarwin,
                payload: ReleaseTarget::X86_64UnknownLinuxMusl,
            })
        );
    }

    #[test]
    fn regular_payload_and_receipt_inputs_are_read_whole() {
        let scratch = Scratch::new();
        let worktree = scratch.join("worktree");
        let common = scratch.join("common");
        let layout = Layout::new(
            &worktree,
            common.join(".spice/proof"),
            ReleaseTarget::Aarch64AppleDarwin,
        );
        let payload = payload(ReleaseTarget::Aarch64AppleDarwin, "readable");
        let receipt = layout.receipt_for(&payload).expect("matching payload");
        fs::create_dir_all(layout.payload_path().parent().expect("payload parent"))
            .expect("create payload parent");
        fs::create_dir_all(receipt.path().parent().expect("receipt parent"))
            .expect("create receipt parent");
        fs::write(layout.payload_path(), b"payload bytes").expect("write payload");
        fs::write(receipt.path(), b"receipt bytes").expect("write receipt");

        assert_eq!(
            layout.read_payload().expect("read payload"),
            b"payload bytes"
        );
        assert_eq!(receipt.read().expect("read receipt"), b"receipt bytes");
    }

    #[test]
    fn exact_missing_inputs_are_named_while_planted_decoys_are_ignored() {
        let scratch = Scratch::new();
        let worktree = scratch.join("worktree");
        let common = scratch.join("common");
        let layout = Layout::new(
            &worktree,
            common.join(".spice/proof"),
            ReleaseTarget::Aarch64AppleDarwin,
        );
        let payload = payload(ReleaseTarget::Aarch64AppleDarwin, "missing");
        let receipt = layout.receipt_for(&payload).expect("matching payload");
        let payload_decoy = worktree.join("dist/stage/targets/latest/payload.json");
        let receipt_decoy = common.join(".spice/proof/latest/receipt.json");
        fs::create_dir_all(payload_decoy.parent().expect("payload decoy parent"))
            .expect("create payload decoy parent");
        fs::create_dir_all(receipt_decoy.parent().expect("receipt decoy parent"))
            .expect("create receipt decoy parent");
        fs::write(&payload_decoy, b"decoy payload").expect("write payload decoy");
        fs::write(&receipt_decoy, b"decoy receipt").expect("write receipt decoy");

        assert!(matches!(
            layout.read_payload(),
            Err(InputError::Missing {
                input: Input::Payload,
                path
            }) if path == layout.payload_path()
        ));
        assert!(matches!(
            receipt.read(),
            Err(InputError::Missing { input: Input::Receipt, path }) if path == receipt.path()
        ));
        assert_eq!(
            fs::read(payload_decoy).expect("payload decoy remains"),
            b"decoy payload"
        );
        assert_eq!(
            fs::read(receipt_decoy).expect("receipt decoy remains"),
            b"decoy receipt"
        );
    }

    #[test]
    fn non_regular_inputs_are_distinguished_for_payload_and_receipt() {
        let scratch = Scratch::new();
        let layout = Layout::new(
            scratch.join("worktree"),
            scratch.join("common/.spice/proof"),
            ReleaseTarget::Aarch64AppleDarwin,
        );
        let payload = payload(ReleaseTarget::Aarch64AppleDarwin, "directory inputs");
        let receipt = layout.receipt_for(&payload).expect("matching payload");
        fs::create_dir_all(layout.payload_path()).expect("create payload directory");
        fs::create_dir_all(receipt.path()).expect("create receipt directory");

        assert!(matches!(
            layout.read_payload(),
            Err(InputError::NotRegular {
                input: Input::Payload,
                path
            }) if path == layout.payload_path()
        ));
        assert!(matches!(
            receipt.read(),
            Err(InputError::NotRegular { input: Input::Receipt, path }) if path == receipt.path()
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_inputs_are_distinguished_for_payload_and_receipt() {
        use std::os::unix::fs::symlink;

        let scratch = Scratch::new();
        let layout = Layout::new(
            scratch.join("worktree"),
            scratch.join("common/.spice/proof"),
            ReleaseTarget::Aarch64AppleDarwin,
        );
        let payload = payload(ReleaseTarget::Aarch64AppleDarwin, "symlink inputs");
        let receipt = layout.receipt_for(&payload).expect("matching payload");
        let target = scratch.join("ordinary file");
        fs::write(&target, b"ordinary bytes").expect("write symlink target");
        fs::create_dir_all(layout.payload_path().parent().expect("payload parent"))
            .expect("create payload parent");
        fs::create_dir_all(receipt.path().parent().expect("receipt parent"))
            .expect("create receipt parent");
        symlink(&target, layout.payload_path()).expect("link payload");
        symlink(&target, receipt.path()).expect("link receipt");

        assert!(matches!(
            layout.read_payload(),
            Err(InputError::Symlink {
                input: Input::Payload,
                path
            }) if path == layout.payload_path()
        ));
        assert!(matches!(
            receipt.read(),
            Err(InputError::Symlink { input: Input::Receipt, path }) if path == receipt.path()
        ));
    }

    #[test]
    fn unreadable_inputs_preserve_their_kind_path_and_io_source() {
        let scratch = Scratch::new();
        let path = scratch.join("regular input");
        fs::write(&path, b"ordinary bytes").expect("write regular input");

        for input in [Input::Payload, Input::Receipt] {
            let error = read_exact_with(&path, input, |_| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "planted unreadable input",
                ))
            })
            .expect_err("unreadable input refuses");

            assert!(matches!(
                error,
                InputError::Unreadable { input: observed, path: observed_path, source }
                    if observed == input
                        && observed_path == path
                        && source.kind() == io::ErrorKind::PermissionDenied
            ));
        }
    }

    fn payload(target: ReleaseTarget, marker: &str) -> TargetPayload {
        let digest = Digest::compute(Domain::DistArtifact, marker.as_bytes());
        TargetPayload {
            version: wire::manifest::TARGET_PAYLOAD_VERSION,
            target,
            source: digest.clone(),
            archive: digest.clone(),
            app: digest.clone(),
            canvas: digest.clone(),
            native: digest.clone(),
            symbols: None,
            pins: BuildPins {
                rust: marker.to_owned(),
                wasm_target: marker.to_owned(),
                trunk: marker.to_owned(),
                proof_container: marker.to_owned(),
            },
            pins_digest: digest.clone(),
            inventory: Vec::<InventoryEntry>::new(),
            inventory_digest: digest,
        }
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "spice-proof-layout-{}-{}",
                std::process::id(),
                NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create layout scratch");
            Self(path)
        }

        fn join(&self, path: impl AsRef<Path>) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove layout scratch");
        }
    }
}
