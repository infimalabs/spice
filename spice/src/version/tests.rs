use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "spice-version-app-selection-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).expect("fixture root creates exclusively");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("fixture removes");
    }
}

fn linked_generation() -> Digest {
    Digest::compute(Domain::DistArtifact, b"linked version app")
}

static ARCHIVE: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let shell = format!(
        "<main data-app-generation=\"{}\"></main>",
        linked_generation()
    );
    let path = format!(
        "{}/{}",
        serve::canvas::APP_DIRECTORY,
        wire::manifest::APP_HTML
    );
    serve::canvas::gzip::write([
        (path.as_str(), shell.as_bytes()),
        (serve::canvas::BUILD, b"{\"product\":\"0.1.0\"}".as_slice()),
    ])
    .expect("fixture rack writes")
});

fn seed_stage(root: &Path) -> Digest {
    let generation = Digest::compute(Domain::DistArtifact, b"development version app");
    let stage = root.join(serve::APP_STAGE);
    let directory = stage
        .join(serve::APP_STAGE_GENERATIONS)
        .join(generation.as_str());
    fs::create_dir_all(&directory).expect("stage directory creates");
    for asset in serve::APP.assets() {
        fs::write(directory.join(asset.file()), asset.file()).expect("stage asset writes");
    }
    fs::write(
        stage.join(serve::APP_STAGE_CURRENT),
        format!("{generation}\n"),
    )
    .expect("stage pointer writes");
    generation
}

#[test]
fn linked_app_identity_wins_over_a_different_or_broken_worktree_stage() {
    let root = Scratch::new();
    let staged = seed_stage(&root.0);
    let linked = Ok(EmbeddedArchive::admit(&ARCHIVE).expect("fixture archive admits"));
    let expected = linked_generation();
    assert_ne!(staged, expected);
    assert_eq!(app_generation(&root.0, Some(&linked)).unwrap(), expected);
    fs::write(
        root.0.join(serve::APP_STAGE).join(serve::APP_STAGE_CURRENT),
        "invalid stage identity",
    )
    .expect("stage pointer corrupts");
    assert_eq!(app_generation(&root.0, Some(&linked)).unwrap(), expected);
}

#[test]
fn rejected_linked_archive_refuses_even_with_a_valid_worktree_stage() {
    let root = Scratch::new();
    seed_stage(&root.0);
    let rejected = EmbeddedArchive::admit(b"not a rack archive");
    assert!(app_generation(&root.0, Some(&rejected)).is_err());
}

/// The linked rack's build record names the product version; an unlinked
/// build names the tracked major as development; a refused linked rack names
/// no version, with nothing standing in for it.
#[test]
fn the_linked_build_record_names_the_product_version_or_nothing_does() {
    let development = || ProductVersion::parse("0.0.0-dev").expect("fixture version");
    let linked = Ok(EmbeddedArchive::admit(&ARCHIVE).expect("fixture archive admits"));
    assert_eq!(
        product_version(Some(&linked), || unreachable!(
            "a linked rack names the version"
        ))
        .expect("linked record names the version")
        .as_str(),
        "0.1.0"
    );
    assert_eq!(product_version(None, development).unwrap(), development());
    let rejected = EmbeddedArchive::admit(b"not a rack archive");
    assert!(product_version(Some(&rejected), development).is_err());
}

#[test]
fn an_unlinked_build_requires_a_real_readable_stage() {
    let root = Scratch::new();
    assert!(app_generation(&root.0, None).is_err());
    let staged = seed_stage(&root.0);
    assert_eq!(app_generation(&root.0, None).unwrap(), staged);
    fs::write(
        root.0.join(serve::APP_STAGE).join(serve::APP_STAGE_CURRENT),
        "invalid stage identity",
    )
    .expect("stage pointer corrupts");
    assert!(app_generation(&root.0, None).is_err());
}
