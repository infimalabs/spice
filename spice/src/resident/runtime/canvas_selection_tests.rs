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
            "spice-canvas-selection-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).expect("fixture root creates");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("fixture root removes");
    }
}

fn generation() -> wire::Digest {
    wire::Digest::compute(wire::Domain::DistArtifact, b"resident canvas selection")
}

static EMBEDDED_ARCHIVE: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let shell = format!("<main data-app-generation=\"{}\"></main>", generation());
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

fn embedded_archive() -> serve::canvas::EmbeddedArchive {
    serve::canvas::EmbeddedArchive::admit(&EMBEDDED_ARCHIVE).expect("fixture archive admits")
}

fn seed_stage(root: &Path) {
    let generation = generation();
    let stage = root.join(serve::APP_STAGE);
    let directory = stage
        .join(serve::APP_STAGE_GENERATIONS)
        .join(generation.as_str());
    fs::create_dir_all(&directory).expect("fixture generation creates");
    for asset in serve::APP.assets() {
        fs::write(directory.join(asset.file()), asset.file()).expect("fixture asset writes");
    }
    fs::write(
        stage.join(serve::APP_STAGE_CURRENT),
        format!("{generation}\n"),
    )
    .expect("fixture pointer writes");
}

#[test]
fn installed_and_staged_residents_select_their_only_canvas_sources() {
    let root = Scratch::new();
    seed_stage(root.path());
    let linked = Ok(embedded_archive());

    let installed = AppSelection::Installed
        .resolve_with(root.path(), Some(&linked))
        .expect("installed canvas resolves");
    assert!(matches!(
        installed,
        serve::canvas::CanvasAssets::EmbeddedArchive(_)
    ));

    let staged = AppSelection::Staged
        .resolve_with(root.path(), None)
        .expect("staged canvas resolves");
    assert!(matches!(
        staged,
        serve::canvas::CanvasAssets::DevelopmentDirectory(_)
    ));
}

#[test]
fn an_installed_resident_refuses_absent_or_invalid_linked_canvas_bytes() {
    let root = Scratch::new();
    let Err(Outcome::Fault(absent)) = AppSelection::Installed.resolve_with(root.path(), None)
    else {
        panic!("an absent linked archive must fault");
    };
    assert_eq!(absent.reason, "serve::canvas-archive-absent");

    let invalid = Err(rack::Error::Structure("fixture archive is invalid"));
    let Err(Outcome::Fault(invalid)) =
        AppSelection::Installed.resolve_with(root.path(), Some(&invalid))
    else {
        panic!("an invalid linked archive must fault");
    };
    assert_eq!(invalid.reason, "serve::canvas-archive-invalid");
}
