//! Exact worktree-source identity for one browser-app compilation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use wire::{Digest, Domain};

use super::AppBuildError;

const SOURCE_FILES: [&str; 22] = [
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain.toml",
    "app/Cargo.toml",
    "app/index.html",
    "app/chrome.css",
    "app/compose_overlay.css",
    "app/composer.css",
    "app/lane_mosaic.css",
    "app/mosaic.css",
    "app/responsive.css",
    "app/spice_sidebar.css",
    "app/style.css",
    "app/team_controls.css",
    "app/work.css",
    "app/work-map.css",
    "wire/Cargo.toml",
    "xtask/Cargo.toml",
    "xtask/src/app.rs",
    "xtask/src/app/rack.rs",
    "xtask/src/app/renderer.rs",
    "xtask/src/app/source.rs",
];
const SOURCE_DIRECTORIES: [&str; 2] = ["app/src", "wire/src"];

/// One observation of every worktree input the browser-app build reads.
pub(super) struct SourceSnapshot {
    inputs: BTreeMap<String, Digest>,
    identity: Digest,
    compiled: Digest,
}

impl SourceSnapshot {
    /// Capture the worktree bytes before compilation starts.
    pub(super) fn capture(root: &Path) -> Result<Self, AppBuildError> {
        let mut inputs = BTreeMap::new();
        let mut framed = Vec::new();
        let mut compiled = Vec::new();
        for relative in source_paths(root)? {
            let path = root.join(&relative);
            let bytes = fs::read(&path).map_err(|source| AppBuildError::Io { path, source })?;
            let start = framed.len();
            framed.extend_from_slice(&(relative.len() as u64).to_be_bytes());
            framed.extend_from_slice(relative.as_bytes());
            framed.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
            framed.extend_from_slice(&bytes);
            if !copied_asset(&relative) {
                compiled.extend_from_slice(&framed[start..]);
            }
            inputs.insert(relative, Digest::compute(Domain::GateFile, &bytes));
        }
        Ok(Self {
            inputs,
            identity: Digest::compute(Domain::DistInventory, &framed),
            compiled: Digest::compute(Domain::DistInventory, &compiled),
        })
    }

    /// Every input, assets included: what one published generation is from.
    pub(super) const fn identity(&self) -> &Digest {
        &self.identity
    }

    /// Only the inputs compilation reads, so the Wasm carries this identity and
    /// an asset-only edit leaves rustc's environment, and its build, unchanged.
    pub(super) const fn compiled(&self) -> &Digest {
        &self.compiled
    }

    /// Refuse to pair output with an identity captured before a source moved.
    pub(super) fn verify_unchanged(&self, root: &Path) -> Result<(), AppBuildError> {
        let observed = Self::capture(root)?;
        let changed = disagreeing_paths(&self.inputs, &observed.inputs);
        if changed.is_empty() {
            return Ok(());
        }
        Err(AppBuildError::SourceIdentity(format!(
            "inputs changed during compilation: {}",
            changed.join(", ")
        )))
    }
}

fn source_paths(root: &Path) -> Result<Vec<String>, AppBuildError> {
    let mut paths = SOURCE_FILES
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for relative in &paths {
        admit_regular(&root.join(relative))?;
    }
    for directory in SOURCE_DIRECTORIES {
        collect_directory(root, &root.join(directory), &mut paths)?;
    }
    let renderer = root.join(super::renderer::DIRECTORY);
    collect_directory(root, &renderer, &mut paths)?;
    paths.sort();
    Ok(paths)
}

/// Whether Trunk or staging copies `relative` into the generation without the
/// compiler reading it: the page shell, its stylesheets, and renderer sources.
fn copied_asset(relative: &str) -> bool {
    let stylesheet = relative
        .strip_prefix("app/")
        .is_some_and(|name| !name.contains('/') && name.ends_with(".css"));
    let renderer = relative
        .strip_prefix(super::renderer::DIRECTORY)
        .is_some_and(|rest| rest.starts_with('/'));
    stylesheet || renderer || relative == "app/index.html"
}

pub(super) fn collect_directory(
    root: &Path,
    directory: &Path,
    paths: &mut Vec<String>,
) -> Result<(), AppBuildError> {
    let metadata = fs::symlink_metadata(directory).map_err(|source| AppBuildError::Io {
        path: directory.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_dir() {
        return Err(source_path_error(directory, "is not a directory"));
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|source| AppBuildError::Io {
            path: directory.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| AppBuildError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| AppBuildError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_dir() {
            collect_directory(root, &path, paths)?;
        } else if metadata.file_type().is_file() {
            paths.push(relative_spelling(root, &path)?);
        } else {
            return Err(source_path_error(
                &path,
                "is not a regular file or directory",
            ));
        }
    }
    Ok(())
}

fn admit_regular(path: &Path) -> Result<(), AppBuildError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| AppBuildError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(source_path_error(path, "is not a regular file"))
    }
}

fn relative_spelling(root: &Path, path: &Path) -> Result<String, AppBuildError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        source_path_error(path, "escaped the worktree while sources were collected")
    })?;
    let mut spelling = String::with_capacity(relative.as_os_str().len());
    for component in relative.components() {
        let component = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| AppBuildError::NonUtf8(PathBuf::from(relative)))?;
        if !spelling.is_empty() {
            spelling.push('/');
        }
        spelling.push_str(component);
    }
    Ok(spelling)
}

fn source_path_error(path: &Path, reason: &str) -> AppBuildError {
    AppBuildError::SourceIdentity(format!("{} {reason}", path.display()))
}

fn disagreeing_paths(
    expected: &BTreeMap<String, Digest>,
    observed: &BTreeMap<String, Digest>,
) -> Vec<String> {
    let mut paths = expected
        .keys()
        .chain(observed.keys())
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths.retain(|path| expected.get(path) != observed.get(path));
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// How many names one fixture tries before giving up on an exclusive root.
    const SCRATCH_ATTEMPTS: usize = 100;

    /// A planted source closure this fixture is the only holder of.
    struct SourceTree(PathBuf);

    impl SourceTree {
        /// One scratch root on a name nothing else already holds.
        ///
        /// `create_dir` refuses a directory that exists rather than opening it,
        /// so retrying under a fresh name is what makes the root exclusive: this
        /// run only ever owns a name its own `create_dir` succeeded on. The name
        /// carries the clock rather than the process id because ids are reused
        /// across the lanes sharing this temporary directory and the clock is
        /// not, so a peer's live fixture is never named the same as this one.
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            for _ in 0..SCRATCH_ATTEMPTS {
                let stamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("the clock reads after the epoch")
                    .as_nanos();
                let root = std::env::temp_dir().join(format!(
                    "spice-xtask-app-source-snapshot-{stamp}-{}",
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&root) {
                    Ok(()) => return Self::planted(root),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("exclusive scratch root is created: {error}"),
                }
            }
            panic!("no exclusive scratch root was available in {SCRATCH_ATTEMPTS} attempts");
        }

        /// The declared source closure, planted under a root this run owns.
        fn planted(root: PathBuf) -> Self {
            for relative in SOURCE_FILES {
                let path = root.join(relative);
                fs::create_dir_all(path.parent().expect("source file has a parent"))
                    .expect("create source-file parent");
                fs::write(path, relative).expect("write source-file fixture");
            }
            for directory in SOURCE_DIRECTORIES {
                let path = root.join(directory).join("lib.rs");
                fs::create_dir_all(path.parent().expect("source module has a parent"))
                    .expect("create source-module parent");
                fs::write(path, directory).expect("write source-module fixture");
            }
            let renderer = root.join(super::super::renderer::DIRECTORY);
            fs::create_dir_all(&renderer).expect("create renderer-source directory");
            fs::write(renderer.join("renderer-000.html"), "renderer shell")
                .expect("write renderer-source fixture");
            Self(root)
        }
    }

    impl Drop for SourceTree {
        fn drop(&mut self) {
            // A root that outlives its test is a name the next run can collide
            // with, so remove it even where a panic is unwinding through here.
            // Removal is allowed to fail rather than replace the failure the
            // test is already carrying with one about its own cleanup.
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn host_path_separators_do_not_change_spelling_or_order() {
        let root = Path::new("fixture");
        let nested = root.join("app").join("src").join("nested").join("lib.rs");
        let mut paths = vec![
            "app/src/z.rs".to_owned(),
            relative_spelling(root, &nested).expect("spell nested source path"),
            "app/src/a.rs".to_owned(),
        ];
        paths.sort();

        assert_eq!(
            paths,
            ["app/src/a.rs", "app/src/nested/lib.rs", "app/src/z.rs"]
        );
    }

    #[test]
    fn untracked_source_arrival_and_movement_change_the_identity() {
        let tree = SourceTree::new();
        let root = &tree.0;
        let before = SourceSnapshot::capture(root).expect("capture fixture sources");
        let untracked = root.join("app/src/worktree_only.rs");
        fs::write(&untracked, "first worktree-only source").expect("write worktree-only source");

        let error = before
            .verify_unchanged(root)
            .expect_err("an arriving source must move the identity");
        assert!(
            error.to_string().contains("app/src/worktree_only.rs"),
            "the arriving source was not named: {error}"
        );

        let captured = SourceSnapshot::capture(root).expect("capture worktree-only source");
        fs::write(untracked, "different worktree-only source").expect("move worktree-only source");
        let error = captured
            .verify_unchanged(root)
            .expect_err("a changed source must move the identity");
        assert!(
            error.to_string().contains("app/src/worktree_only.rs"),
            "the changed source was not named: {error}"
        );
    }

    #[test]
    fn a_copied_asset_edit_moves_only_the_full_identity() {
        let tree = SourceTree::new();
        let root = &tree.0;
        let before = SourceSnapshot::capture(root).expect("capture fixture sources");
        fs::write(root.join("app/compose_overlay.css"), "edited stylesheet")
            .expect("edit stylesheet");
        let styled = SourceSnapshot::capture(root).expect("capture edited stylesheet");
        assert_ne!(styled.identity(), before.identity());
        assert_eq!(styled.compiled(), before.compiled());

        fs::write(root.join("app/src/lib.rs"), "edited module").expect("edit module");
        let compiled = SourceSnapshot::capture(root).expect("capture edited module");
        assert_ne!(compiled.identity(), styled.identity());
        assert_ne!(compiled.compiled(), styled.compiled());
    }

    #[test]
    fn every_source_inventory_difference_is_named() {
        let digest = |bytes| Digest::compute(Domain::GateFile, bytes);
        let expected = BTreeMap::from([
            ("app/src/lib.rs".to_owned(), digest(b"one")),
            ("wire/src/lib.rs".to_owned(), digest(b"two")),
        ]);
        let observed = BTreeMap::from([
            ("app/src/lib.rs".to_owned(), digest(b"three")),
            ("app/style.css".to_owned(), digest(b"four")),
        ]);

        assert_eq!(
            disagreeing_paths(&expected, &observed),
            ["app/src/lib.rs", "app/style.css", "wire/src/lib.rs"]
        );
    }
}
