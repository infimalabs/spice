//! Product identity shared by native version surfaces.

use std::fs::File;
use std::path::Path;
use std::sync::LazyLock;

use serve::canvas::EmbeddedArchive;
use wire::manifest::ProductVersion;
use wire::{Digest, Domain, Fault, FaultClass};

pub use dist::{
    SourceIdentity, build_info, build_record_bytes, head_build_info, source_counter_sum,
    source_identity, source_version,
};

/// Identity of the executable this process is running.
#[derive(Debug)]
pub struct Running {
    /// Digest of the executable bytes.
    pub binary: Digest,
    /// Product version derived from the tracked version and source commit.
    pub product: ProductVersion,
}

/// The product version this executable reports: the one the build wrote into
/// the linked rack's build record. A build linking no rack is bound to no
/// commit and reports its tracked major and compiled counter sum with patch 0,
/// marked `-dev`.
static PRODUCT: LazyLock<ProductVersion> = LazyLock::new(|| {
    product_version(canvas(), || {
        dist::current_tracked_version()
            .expect("the compiled tracked version is repository-validated")
            .product(
                dist::current_counter_sum().expect("the compiled counters are validated"),
                0,
                dist::Build::Dev,
            )
    })
    .unwrap_or_else(|error| {
        panic!("the linked spice rack must admit before it names a version: {error}")
    })
});

/// The product version an executable linking `linked` reports: the admitted
/// rack's build record, or `unlinked` when the build linked no rack. A linked
/// rack that is refused names no version at all.
///
/// # Errors
///
/// The refusal of the linked rack; no other version stands in for it.
pub fn product_version(
    linked: Option<&Result<EmbeddedArchive, rack::Error>>,
    unlinked: impl FnOnce() -> ProductVersion,
) -> Result<ProductVersion, &rack::Error> {
    match linked {
        Some(Ok(archive)) => Ok(archive.build().product.clone()),
        Some(Err(error)) => Err(error),
        None => Ok(unlinked()),
    }
}

static RUNNING: LazyLock<Result<Running, Fault>> = LazyLock::new(capture);

static CANVAS: LazyLock<Option<Result<EmbeddedArchive, rack::Error>>> =
    LazyLock::new(|| linked_bytes().map(EmbeddedArchive::admit));

static RACK_ARCHIVE: LazyLock<Option<Digest>> =
    LazyLock::new(|| linked_bytes().map(|bytes| Digest::compute(Domain::DistArtifact, bytes)));

/// The rack archive bytes linked into this executable, if the build linked one.
fn linked_bytes() -> Option<&'static [u8]> {
    #[cfg(feature = "canvas")]
    let linked = Some(rack::linked());
    #[cfg(not(feature = "canvas"))]
    let linked: Option<&'static [u8]> = None;
    linked
}

/// The product version this executable was built as.
///
/// Fixed at compile time, so naming it does not hash the executable the way
/// [`running`] must.
pub fn product() -> &'static ProductVersion {
    &PRODUCT
}

/// The rack linked into this executable, admitted once per process.
///
/// `None` when the build linked no archive: only the release target build and
/// `spice dev xtask -- install` enable the `canvas` feature and link one.
pub fn canvas() -> Option<&'static Result<EmbeddedArchive, rack::Error>> {
    CANVAS.as_ref()
}

/// The digest of the rack archive bytes this executable links, hashed once
/// per process: the identity a `rack::admitted` fact names.
pub fn rack_archive() -> Option<&'static Digest> {
    RACK_ARCHIVE.as_ref()
}

/// The version report: the product version and the app generation it links.
pub fn report() -> String {
    match canvas() {
        Some(Ok(archive)) => format!("{} app {}", product(), archive.generation()),
        _ => product().to_string(),
    }
}

/// The app generation `spice version` reports.
///
/// An executable that links an archive names that archive's generation and
/// reads nothing from the worktree, as `--version` and the installed resident
/// do. Only a build that links none, which is a development build, names the
/// generation staged in `worktree`.
/// `linked` is the executable's admitted archive state; supplying it explicitly
/// keeps the precedence independent of process-global initialization.
///
/// # Errors
///
/// Refuses a linked archive that failed admission and, for a development
/// build, an absent or malformed staged app.
pub fn app_generation(
    worktree: &Path,
    linked: Option<&Result<EmbeddedArchive, rack::Error>>,
) -> Result<Digest, String> {
    match linked {
        Some(Ok(archive)) => Ok(archive.generation().clone()),
        Some(Err(error)) => Err(error.to_string()),
        None => serve::AppSource::resolve(worktree)
            .and_then(|source| source.current_generation())
            .map(|generation| generation.identity().clone())
            .map_err(|error| error.to_string()),
    }
}

/// The running executable's identity, captured once per process.
///
/// The resident captures it before serving, so an install that later
/// replaces the executable file cannot change what this process reports.
///
/// # Errors
///
/// Reports an environment fault when the running executable could not be
/// resolved or read at capture. No unqualified version is substituted for
/// missing build evidence.
pub fn running() -> Result<&'static Running, Fault> {
    RUNNING.as_ref().map_err(Clone::clone)
}

fn capture() -> Result<Running, Fault> {
    let binary = binary_digest()?;
    Ok(Running {
        binary,
        product: product().clone(),
    })
}

/// Hash the executable without consulting a repository or retaining its bytes.
fn binary_digest() -> Result<Digest, Fault> {
    let executable = std::env::current_exe().map_err(|error| Fault {
        class: FaultClass::Environment,
        reason: "version::binary-unresolved".to_owned(),
        detail: error.to_string(),
    })?;
    let read = || {
        let file = File::open(&executable)?;
        let limit = file.metadata()?.len();
        Digest::compute_reader(Domain::DistArtifact, file, limit).map(|(_, digest)| digest)
    };
    read().map_err(|error: std::io::Error| Fault {
        class: FaultClass::Environment,
        reason: "version::binary-unreadable".to_owned(),
        detail: format!("{}: {error}", executable.display()),
    })
}

#[cfg(test)]
mod tests;
