use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

static APP_ASSETS: [AppAsset; 13] = [
    AppAsset::new("/", wire::manifest::APP_HTML),
    AppAsset::new("/app.module", wire::manifest::APP_MODULE),
    AppAsset::new("/style.css", wire::manifest::APP_STYLE),
    AppAsset::new("/spice_sidebar.css", wire::manifest::SPICE_SIDEBAR_STYLE),
    AppAsset::new("/mosaic.css", wire::manifest::MOSAIC_STYLE),
    AppAsset::new("/team_controls.css", wire::manifest::TEAM_CONTROLS_STYLE),
    AppAsset::new("/lane_mosaic.css", wire::manifest::LANE_MOSAIC_STYLE),
    AppAsset::new("/chrome.css", wire::manifest::CHROME_STYLE),
    AppAsset::new("/composer.css", wire::manifest::COMPOSER_STYLE),
    AppAsset::new("/work.css", wire::manifest::WORK_STYLE),
    AppAsset::new("/work-map.css", wire::manifest::WORK_MAP_STYLE),
    AppAsset::new("/responsive.css", wire::manifest::RESPONSIVE_STYLE),
    AppAsset::new("/app_bg.wasm", wire::manifest::APP_WASM),
];

/// The content type of an HTML document.
pub(crate) const DOCUMENT_CONTENT_TYPE: &str = "text/html; charset=utf-8";

/// The content type a path's extension declares: Spice's one table for every
/// file it serves or reads. Bytes are never sniffed, and an extension the
/// table does not name declares nothing.
#[must_use]
pub fn content_type(path: &str) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let (_, extension) = file.rsplit_once('.')?;
    extension_content_type(extension)
}

fn extension_content_type(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "html" => DOCUMENT_CONTENT_TYPE,
        "css" => "text/css; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        "mjs" | "module" | "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "toml" => "application/toml",
        "wasm" => "application/wasm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ => return None,
    })
}

/// The table's content type for one renderer kind.
fn renderer_content_type(kind: wire::manifest::renderer::Kind) -> &'static str {
    extension_content_type(kind.extension()).expect("every renderer kind's extension is tabled")
}

/// The sole browser app generation store, built and served per worktree.
pub const APP_STAGE: &str = "dist/stage-app";
/// Atomic pointer to the active immutable lane-app generation.
pub const APP_STAGE_CURRENT: &str = "current";
/// Immutable lane-app generations beneath [`APP_STAGE`].
pub const APP_STAGE_GENERATIONS: &str = "generations";
/// Route prefix that pins a page's dependent assets to one generation.
pub const APP_GENERATION_ROUTE: &str = "/_spice/app";
/// Generations retained in resident memory: the active page and the page that
/// can still be loading across one atomic publication.
const APP_GENERATION_CACHE_CAPACITY: usize = 2;

/// One route in the closed browser app inventory.
#[derive(Clone, Copy)]
pub struct AppAsset {
    path: &'static str,
    file: &'static str,
}

impl AppAsset {
    const fn new(path: &'static str, file: &'static str) -> Self {
        Self { path, file }
    }

    #[must_use]
    pub const fn path(self) -> &'static str {
        self.path
    }

    /// The staged file this route serves.
    #[must_use]
    pub const fn file(self) -> &'static str {
        self.file
    }

    /// The table's content type for this route's file.
    #[must_use]
    pub fn content_type(self) -> &'static str {
        content_type(self.file).expect("every inventory file's extension is tabled")
    }
}

impl fmt::Debug for AppAsset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAsset")
            .field("path", &self.path)
            .field("file", &self.file)
            .finish()
    }
}

/// The required browser shell route inventory.
#[derive(Clone, Copy)]
pub struct App {
    assets: &'static [AppAsset],
}

/// The base files every lane build must produce.
pub const APP: App = App {
    assets: &APP_ASSETS,
};

impl App {
    /// Required shell assets in their closed route order; use [`AppGeneration::files`] for a complete generation.
    #[must_use]
    pub const fn assets(self) -> &'static [AppAsset] {
        self.assets
    }

    /// Resolve one exact route. Unknown paths have no shell fallback.
    #[must_use]
    pub fn asset(self, path: &str) -> Option<AppAsset> {
        self.assets.iter().copied().find(|asset| asset.path == path)
    }

    /// Resolve one required shell file, excluding generation-declared renderers.
    #[must_use]
    pub fn asset_file(self, file: &str) -> Option<AppAsset> {
        self.assets.iter().copied().find(|asset| asset.file == file)
    }
}

impl fmt::Debug for App {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("App")
            .field("assets", &self.assets)
            .finish()
    }
}

/// The lane-built browser app served by one app generation.
#[derive(Clone)]
pub struct AppSource {
    stage: PathBuf,
    generations: Arc<Mutex<GenerationCache>>,
}

/// One immutable, complete lane-app generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppGeneration {
    directory: PathBuf,
    identity: wire::Digest,
    assets: Arc<BTreeMap<&'static str, ServedAsset>>,
    renderers: Arc<BTreeMap<wire::manifest::renderer::File, ServedAsset>>,
}

#[derive(Debug, Default)]
struct GenerationCache {
    generations: BTreeMap<wire::Digest, AppGeneration>,
    oldest_first: VecDeque<wire::Digest>,
    current: Option<wire::Digest>,
}

impl AppSource {
    /// Resolve the one app stage this worktree must serve.
    ///
    /// The current pointer remains the only stage freshness check on the hot
    /// path. A new identity is admitted under the generation-cache lock: its
    /// closed inventory is validated, then all declared files and their digests
    /// are loaded exactly once. Requests for an admitted identity clone shared byte
    /// handles and never reopen or rehash its files.
    ///
    /// # Errors
    ///
    /// Returns [`AppSourceError::Incomplete`] when the pointer or any file in
    /// its named generation is absent.
    pub fn resolve(worktree: &Path) -> Result<Self, AppSourceError> {
        let source = Self {
            stage: worktree.join(APP_STAGE),
            generations: Arc::new(Mutex::new(GenerationCache::default())),
        };
        source.current_generation()?;
        Ok(source)
    }

    /// Resolve the generation named by the one atomic publication pointer.
    ///
    /// # Errors
    ///
    /// Returns a named pointer, identity, inventory, or file-system failure.
    pub fn current_generation(&self) -> Result<AppGeneration, AppSourceError> {
        let pointer = self.stage.join(APP_STAGE_CURRENT);
        let text =
            std::fs::read_to_string(&pointer).map_err(|source| AppSourceError::Incomplete {
                directory: self.stage.clone(),
                missing: vec![APP_STAGE_CURRENT.to_owned()],
                source: Some(source),
            })?;
        let spelling = text.strip_suffix('\n').ok_or_else(|| {
            AppSourceError::Generation(format!(
                "{} is not one newline-terminated generation identity",
                pointer.display()
            ))
        })?;
        let identity = wire::Digest::from_str(spelling).map_err(|error| {
            AppSourceError::Generation(format!("{}: {error}", pointer.display()))
        })?;
        self.generation(identity, true)
    }

    fn generation(
        &self,
        identity: wire::Digest,
        current: bool,
    ) -> Result<AppGeneration, AppSourceError> {
        let mut cache = self
            .generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(generation) = cache.generations.get(&identity).cloned() {
            if current {
                cache.current = Some(identity.clone());
            }
            cache.touch(&identity);
            return Ok(generation);
        }

        let directory = self
            .stage
            .join(APP_STAGE_GENERATIONS)
            .join(identity.as_str());
        let inventory = validate_generation(&directory)?;
        let assets = APP
            .assets()
            .iter()
            .map(|asset| {
                let path = directory.join(asset.file());
                let bytes = if asset.file() == wire::manifest::APP_HTML {
                    Arc::clone(&inventory.html)
                } else {
                    Arc::<[u8]>::from(
                        std::fs::read(&path)
                            .map_err(|source| AppSourceError::Unreadable { path, source })?,
                    )
                };
                let digest = wire::Digest::compute(wire::Domain::DistArtifact, &bytes);
                Ok((
                    asset.file(),
                    ServedAsset {
                        bytes,
                        content_type: asset.content_type(),
                        digest: Arc::new(digest),
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, AppSourceError>>()?;
        let mut renderers = BTreeMap::new();
        let mut total = 0_u64;
        for member in inventory.renderers {
            let path = directory.join(member.file.to_string());
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|source| AppSourceError::Unreadable {
                    path: path.clone(),
                    source,
                })?;
            total = total.checked_add(metadata.len()).ok_or_else(|| {
                AppSourceError::Inventory("renderer assets exceed their byte bound".to_owned())
            })?;
            if !metadata.file_type().is_file() || total > wire::manifest::renderer::MAX_BYTES {
                return Err(AppSourceError::Inventory(
                    "renderer assets exceed their byte bound or are not regular files".to_owned(),
                ));
            }
            let bytes = Arc::<[u8]>::from(
                std::fs::read(&path)
                    .map_err(|source| AppSourceError::Unreadable { path, source })?,
            );
            if bytes.len() as u64 != metadata.len()
                || wire::Digest::compute(wire::Domain::InstallArtifact, &bytes) != member.digest
            {
                return Err(AppSourceError::Inventory(
                    "renderer asset differs from its shell declaration".to_owned(),
                ));
            }
            let digest = Arc::new(wire::Digest::compute(wire::Domain::DistArtifact, &bytes));
            renderers.insert(
                member.file,
                ServedAsset {
                    bytes,
                    content_type: renderer_content_type(member.file.kind()),
                    digest,
                },
            );
        }
        let generation = AppGeneration {
            directory,
            identity: identity.clone(),
            assets: Arc::new(assets),
            renderers: Arc::new(renderers),
        };
        cache.insert(identity, generation.clone(), current);
        Ok(generation)
    }

    /// Serve one exact route. Unknown paths have no shell fallback.
    ///
    /// The root reads the current atomic generation pointer. Every dependent
    /// route carries the generation identity emitted into that root, so a
    /// publication between requests cannot mix a page's module, stylesheet,
    /// and Wasm across generations.
    ///
    /// # Errors
    ///
    /// Returns a named generation, inventory, or read error when the current
    /// pointer or one versioned generation cannot supply its complete answer.
    pub fn served(&self, route: &str) -> Result<Option<ServedAsset>, AppSourceError> {
        if route == "/" {
            return self
                .current_generation()?
                .served(APP.asset("/").expect("the closed inventory has a root"))
                .map(Some);
        }
        let Some((identity, file)) = generation_route(route) else {
            return Ok(None);
        };
        if let Some(asset) = APP.asset_file(file) {
            return Ok(self
                .generation(identity, false)?
                .assets
                .get(asset.file())
                .cloned());
        }
        let Some(file) = wire::manifest::renderer::File::parse_filename(file) else {
            return Ok(None);
        };
        Ok(self
            .generation(identity, false)?
            .renderers
            .get(&file)
            .cloned())
    }

    /// Whether this store can still answer routes qualified with `identity`:
    /// it is cached in memory or its immutable directory remains staged.
    pub(crate) fn holds(&self, identity: &wire::Digest) -> bool {
        self.generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generations
            .contains_key(identity)
            || self
                .stage
                .join(APP_STAGE_GENERATIONS)
                .join(identity.as_str())
                .is_dir()
    }
}

impl fmt::Display for AppSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "lane app at {}", self.stage.display())
    }
}

impl fmt::Debug for AppSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppSource")
            .field("stage", &self.stage)
            .finish_non_exhaustive()
    }
}

impl PartialEq for AppSource {
    fn eq(&self, other: &Self) -> bool {
        self.stage == other.stage
    }
}

impl Eq for AppSource {}

impl GenerationCache {
    fn touch(&mut self, identity: &wire::Digest) {
        if let Some(index) = self
            .oldest_first
            .iter()
            .position(|candidate| candidate == identity)
        {
            self.oldest_first.remove(index);
        }
        self.oldest_first.push_back(identity.clone());
    }

    fn insert(&mut self, identity: wire::Digest, generation: AppGeneration, current: bool) {
        self.generations.insert(identity.clone(), generation);
        if current {
            self.current = Some(identity.clone());
        }
        self.touch(&identity);
        while self.generations.len() > APP_GENERATION_CACHE_CAPACITY {
            let oldest_index = self
                .oldest_first
                .iter()
                .position(|candidate| Some(candidate) != self.current.as_ref())
                .expect("a bounded generation cache has a non-current entry");
            let oldest = self
                .oldest_first
                .remove(oldest_index)
                .expect("the selected generation-cache entry exists");
            self.generations.remove(&oldest);
        }
    }
}

impl AppGeneration {
    /// Canonical identity used in every generation-qualified route.
    #[must_use]
    pub const fn identity(&self) -> &wire::Digest {
        &self.identity
    }

    /// Borrow the renderer files owned by this admitted generation.
    pub fn renderers(
        &self,
    ) -> impl Iterator<Item = (wire::manifest::renderer::File, &ServedAsset)> {
        self.renderers.iter().map(|(file, asset)| (*file, asset))
    }

    /// Borrow every exact file in this generation, including lazy renderer assets.
    pub fn files(
        &self,
    ) -> impl Iterator<Item = (std::borrow::Cow<'static, str>, &ServedAsset)> + Clone {
        self.assets
            .iter()
            .map(|(file, asset)| (std::borrow::Cow::Borrowed(*file), asset))
            .chain(
                self.renderers
                    .iter()
                    .map(|(file, asset)| (std::borrow::Cow::Owned(file.to_string()), asset)),
            )
    }

    /// Route that keeps this asset on this immutable generation.
    #[must_use]
    pub fn route(&self, asset: AppAsset) -> String {
        format!("{APP_GENERATION_ROUTE}/{}/{}", self.identity, asset.file())
    }

    /// Clone one shared asset from this admitted immutable generation.
    ///
    /// # Errors
    ///
    /// Returns [`AppSourceError::Inventory`] only if the closed inventory and
    /// the admitted generation disagree, which is an internal invariant.
    pub fn served(&self, asset: AppAsset) -> Result<ServedAsset, AppSourceError> {
        self.assets.get(asset.file()).cloned().ok_or_else(|| {
            AppSourceError::Inventory(format!(
                "admitted generation {} has no {}",
                self.identity,
                asset.file()
            ))
        })
    }
}

pub(crate) fn generation_route(route: &str) -> Option<(wire::Digest, &str)> {
    let rest = route
        .strip_prefix(APP_GENERATION_ROUTE)?
        .strip_prefix('/')?;
    let (identity, file) = rest.split_once('/')?;
    if file.contains('/') {
        return None;
    }
    Some((identity.parse().ok()?, file))
}

struct GenerationInventory {
    renderers: Vec<wire::manifest::renderer::Member>,
    html: Arc<[u8]>,
}

fn validate_generation(directory: &Path) -> Result<GenerationInventory, AppSourceError> {
    let mut observed = std::fs::read_dir(directory)
        .map_err(|source| AppSourceError::Incomplete {
            directory: directory.to_path_buf(),
            missing: APP
                .assets()
                .iter()
                .map(|asset| asset.file().to_owned())
                .collect(),
            source: Some(source),
        })?
        .map(|entry| {
            let entry = entry.map_err(|source| AppSourceError::Unreadable {
                path: directory.to_path_buf(),
                source,
            })?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|source| AppSourceError::Unreadable { path, source })?;
            if !metadata.file_type().is_file() {
                return Err(AppSourceError::Inventory(format!(
                    "{} is not a regular generation file",
                    entry.path().display()
                )));
            }
            entry.file_name().into_string().map_err(|_| {
                AppSourceError::Inventory(format!(
                    "{} contains a non-UTF-8 file name",
                    directory.display()
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    observed.sort();
    let path = directory.join(wire::manifest::APP_HTML);
    if !observed.iter().any(|file| file == wire::manifest::APP_HTML) {
        return Err(AppSourceError::Incomplete {
            directory: directory.to_path_buf(),
            missing: vec![wire::manifest::APP_HTML.to_owned()],
            source: None,
        });
    }
    let html = Arc::<[u8]>::from(
        std::fs::read(&path).map_err(|source| AppSourceError::Unreadable { path, source })?,
    );
    let members = wire::manifest::renderer::from_html(&html)
        .map_err(|error| AppSourceError::Inventory(error.to_owned()))?;
    let mut expected = wire::manifest::renderer::filenames(&members)
        .map(std::borrow::Cow::into_owned)
        .collect::<Vec<_>>();
    expected.sort();
    if observed != expected {
        let missing = expected
            .iter()
            .filter(|file| !observed.contains(file))
            .cloned()
            .collect::<Vec<_>>();
        let unexpected = observed
            .iter()
            .filter(|file| !expected.contains(file))
            .collect::<Vec<_>>();
        if unexpected.is_empty() && !missing.is_empty() {
            return Err(AppSourceError::Incomplete {
                directory: directory.to_path_buf(),
                missing,
                source: None,
            });
        }
        return Err(AppSourceError::Inventory(format!(
            "{} contains {observed:?}, expected {expected:?}",
            directory.display()
        )));
    }
    Ok(GenerationInventory {
        renderers: members,
        html,
    })
}

/// One shared lane-built asset resolved for one request.
#[derive(Clone, Eq, PartialEq)]
pub struct ServedAsset {
    bytes: Arc<[u8]>,
    content_type: &'static str,
    digest: Arc<wire::Digest>,
}

impl ServedAsset {
    /// Media type of the route that produced these bytes.
    #[must_use]
    pub const fn content_type(&self) -> &'static str {
        self.content_type
    }

    /// Content identity under the domain the distribution stage uses.
    #[must_use]
    pub fn digest(&self) -> &wire::Digest {
        self.digest.as_ref()
    }

    /// Borrow the exact response bytes before consuming the asset.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Copy the bytes for build-time consumers that need an owned vector.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes.to_vec()
    }

    /// Move a shared handle to the immutable response bytes into the server.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn into_shared_bytes(self) -> Arc<[u8]> {
        self.bytes
    }
}

impl fmt::Debug for ServedAsset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServedAsset")
            .field("bytes", &self.bytes.len())
            .field("content_type", &self.content_type)
            .field("digest", self.digest())
            .finish()
    }
}

/// Why an app server could not settle or serve its app source.
#[derive(Debug)]
pub enum AppSourceError {
    /// An app stage directory exists without the whole app in it.
    Incomplete {
        /// The directory that was read.
        directory: PathBuf,
        /// Every staged file it does not hold.
        missing: Vec<String>,
        /// File-system failure that made the generation incomplete, if any.
        source: Option<io::Error>,
    },
    /// A generation file cannot be read while that generation is admitted.
    Unreadable {
        /// Exact path that failed.
        path: PathBuf,
        /// Underlying failure.
        source: io::Error,
    },
    /// The closed route inventory itself is invalid.
    Inventory(String),
    /// The atomic generation pointer did not name one canonical identity.
    Generation(String),
}

impl fmt::Display for AppSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete {
                directory, missing, ..
            } => write!(
                formatter,
                "the lane app stage {} is missing {}; build it with `spice dev xtask -- app`",
                directory.display(),
                missing.join(", ")
            ),
            Self::Unreadable { path, source } => {
                write!(
                    formatter,
                    "cannot read staged app {}: {source}",
                    path.display()
                )
            }
            Self::Inventory(reason) => write!(formatter, "app inventory invalid: {reason}"),
            Self::Generation(reason) => {
                write!(formatter, "app generation invalid: {reason}")
            }
        }
    }
}

impl Error for AppSourceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Incomplete { source, .. } => source
                .as_ref()
                .map(|source| source as &(dyn Error + 'static)),
            Self::Unreadable { source, .. } => Some(source),
            Self::Inventory(_) | Self::Generation(_) => None,
        }
    }
}

/// Mode-0600 discovery document for one resident generation.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    /// Published loopback origin.
    pub endpoint: String,
    /// The discovering principal's capability, presented only inside a hello.
    pub capability: wire::CanonicalUuid,
    /// Resident generation fenced by the endpoint and capability.
    pub epoch: wire::CanonicalUuid,
}

impl fmt::Debug for Discovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Discovery")
            .field("endpoint", &self.endpoint)
            .field("capability", &"[redacted]")
            .field("epoch", &self.epoch)
            .finish()
    }
}

impl Discovery {
    /// Atomically publish this discovery document with owner-only permissions.
    ///
    /// The stage is owner-only from its creation, so the capability is never
    /// readable by another principal.
    pub fn publish(&self, path: impl AsRef<Path>) -> Result<(), PublishError> {
        let bytes = wire::encode(self).map_err(|error| PublishError::Encode(error.to_string()))?;
        let destination = atomic::Destination::new(path).map_err(PublishError::Atomic)?;
        let mut staged = destination.stage().map_err(PublishError::Atomic)?;
        staged
            .write_all(&bytes)
            .map_err(|source| PublishError::Io {
                path: staged.temporary_path().to_path_buf(),
                source,
            })?;
        staged.commit().map_err(PublishError::Atomic)
    }
}

/// Discovery encoding or atomic publication failed.
#[derive(Debug)]
pub enum PublishError {
    Encode(String),
    Atomic(atomic::Error),
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for PublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "resident discovery did not encode: {error}"),
            Self::Atomic(error) => error.fmt(formatter),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "resident discovery write failed at {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl Error for PublishError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Encode(_) => None,
            Self::Atomic(error) => Some(error),
            Self::Io { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};

    use super::*;

    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    const FIRST_GENERATION: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";
    const SECOND_GENERATION: &str =
        "2222222222222222222222222222222222222222222222222222222222222222";
    const THIRD_GENERATION: &str =
        "3333333333333333333333333333333333333333333333333333333333333333";

    fn uuid(text: &str) -> wire::CanonicalUuid {
        text.parse().expect("canonical fixture UUID")
    }

    #[test]
    fn app_routes_are_closed_and_typed() {
        assert_eq!(
            APP.assets()
                .iter()
                .map(|asset| (asset.path(), asset.content_type()))
                .collect::<Vec<_>>(),
            [
                ("/", "text/html; charset=utf-8"),
                ("/app.module", "text/javascript; charset=utf-8"),
                ("/style.css", "text/css; charset=utf-8"),
                ("/spice_sidebar.css", "text/css; charset=utf-8"),
                ("/mosaic.css", "text/css; charset=utf-8"),
                ("/team_controls.css", "text/css; charset=utf-8"),
                ("/lane_mosaic.css", "text/css; charset=utf-8"),
                ("/chrome.css", "text/css; charset=utf-8"),
                ("/composer.css", "text/css; charset=utf-8"),
                ("/work.css", "text/css; charset=utf-8"),
                ("/work-map.css", "text/css; charset=utf-8"),
                ("/responsive.css", "text/css; charset=utf-8"),
                ("/app_bg.wasm", "application/wasm"),
            ]
        );
        assert!(APP.asset("/missing").is_none());
    }

    /// Renderer kinds answer the table's spellings, and a path whose final
    /// segment has no extension the table names declares no type.
    #[test]
    fn the_content_type_table_types_renderers_and_refuses_unknown_extensions() {
        use wire::manifest::renderer::Kind;
        assert_eq!(
            [
                Kind::Document,
                Kind::Module,
                Kind::Style,
                Kind::Text,
                Kind::Woff,
                Kind::Woff2,
            ]
            .map(super::renderer_content_type),
            [
                "text/html; charset=utf-8",
                "text/javascript; charset=utf-8",
                "text/css; charset=utf-8",
                "text/plain; charset=utf-8",
                "font/woff",
                "font/woff2",
            ]
        );
        assert_eq!(
            super::content_type("docs/a.b/guide.md"),
            Some("text/markdown; charset=utf-8")
        );
        for untyped in ["notes.xyz", "README", "docs.md/README", "page.HTML", ""] {
            assert_eq!(super::content_type(untyped), None, "{untyped:?}");
        }
    }

    /// A worktree with nothing in it yet.
    fn worktree() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "spice-app-source-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("fixture worktree");
        root
    }

    /// Fill that worktree's app stage with exactly the named files.
    fn staged_as(root: &Path, identity: &str, files: &[(&str, Vec<u8>)]) -> PathBuf {
        let stage = root.join(APP_STAGE);
        let generation = stage.join(APP_STAGE_GENERATIONS).join(identity);
        fs::create_dir_all(&generation).expect("fixture generation");
        for (file, bytes) in files {
            fs::write(generation.join(file), bytes).expect("fixture asset");
        }
        fs::write(stage.join(APP_STAGE_CURRENT), format!("{identity}\n")).expect("fixture pointer");
        generation
    }

    fn staged(root: &Path, files: &[(&str, Vec<u8>)]) -> PathBuf {
        staged_as(root, FIRST_GENERATION, files)
    }

    /// Bytes for every staged file, each naming itself, so no route can be
    /// served from another route's file without the assertion seeing it.
    fn app() -> Vec<(&'static str, Vec<u8>)> {
        APP.assets()
            .iter()
            .map(|asset| {
                (
                    asset.file(),
                    format!("locally built {}", asset.file()).into_bytes(),
                )
            })
            .collect()
    }

    #[test]
    fn a_worktree_without_a_lane_stage_is_incomplete() {
        let root = worktree();

        let error = AppSource::resolve(&root).expect_err("an absent stage refuses");
        let AppSourceError::Incomplete {
            directory, missing, ..
        } = error
        else {
            panic!("an absent stage refused as {error:?}");
        };
        assert_eq!(directory, root.join(APP_STAGE));
        assert_eq!(missing, [APP_STAGE_CURRENT]);
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn one_page_generation_remains_addressable_after_atomic_publication() {
        let root = worktree();
        let first = staged(&root, &app());
        let stage = root.join(APP_STAGE);

        let source = AppSource::resolve(&root).expect("a complete stage resolves");
        let first_generation = source
            .current_generation()
            .expect("first generation resolves");

        assert_eq!(
            source.to_string(),
            format!("lane app at {}", stage.display())
        );
        let served = source
            .served("/")
            .expect("staged files are readable")
            .expect("the root route is served");
        assert_eq!(served.content_type(), "text/html; charset=utf-8");
        assert_eq!(served.bytes(), b"locally built app.html");
        let wasm_route = first_generation.route(
            APP.asset("/app_bg.wasm")
                .expect("the closed inventory has wasm"),
        );
        let wasm = source
            .served(&wasm_route)
            .expect("staged files are readable")
            .expect("the wasm route is served");
        assert_eq!(wasm.content_type(), "application/wasm");
        assert_eq!(wasm.bytes(), b"locally built app_bg.wasm");

        let mut rebuilt = app();
        rebuilt
            .iter_mut()
            .find(|(file, _)| *file == "app.html")
            .expect("fixture shell")
            .1 = b"rebuilt".to_vec();
        staged_as(&root, SECOND_GENERATION, &rebuilt);

        let served = source
            .served("/")
            .expect("staged files are readable")
            .expect("the root route is served");
        assert_eq!(
            served.digest(),
            &wire::Digest::compute(wire::Domain::DistArtifact, b"rebuilt")
        );
        assert_eq!(served.bytes(), b"rebuilt");
        assert_eq!(
            source
                .served(&wasm_route)
                .expect("the first generation remains readable")
                .expect("the first wasm route remains admitted")
                .bytes()
                .to_vec(),
            b"locally built app_bg.wasm",
            "publication mixed the first page's dependent asset with the second generation"
        );
        assert!(first.is_dir(), "the first immutable generation moved");
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn an_app_stage_missing_files_refuses_rather_than_mixing_sources() {
        let root = worktree();
        let whole = app();
        let stage = staged(&root, &whole[..2]);

        let error = AppSource::resolve(&root).expect_err("a partial stage refuses");

        let AppSourceError::Incomplete {
            directory, missing, ..
        } = &error
        else {
            panic!("a partial stage refused as {error:?}");
        };
        assert_eq!(directory, &stage);
        assert_eq!(
            missing,
            &[
                "app_bg.wasm",
                "chrome.css",
                "composer.css",
                "lane_mosaic.css",
                "mosaic.css",
                "responsive.css",
                "spice_sidebar.css",
                "style.css",
                "team_controls.css",
                "work-map.css",
                "work.css"
            ]
        );
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn an_admitted_generation_survives_loss_of_its_staged_files() {
        let root = worktree();
        let stage = staged(&root, &app());
        let source = AppSource::resolve(&root).expect("a complete stage resolves");
        let generation = source
            .current_generation()
            .expect("current generation resolves");

        fs::remove_file(stage.join("style.css")).expect("fixture removes one staged file");

        let route = generation.route(
            APP.asset("/style.css")
                .expect("the closed inventory has a stylesheet"),
        );
        let served = source
            .served(&route)
            .expect("an admitted generation no longer needs the staged file")
            .expect("the admitted stylesheet remains routed");
        assert_eq!(served.bytes(), b"locally built style.css");
        assert_eq!(
            source
                .served("/")
                .expect("the admitted generation remains complete")
                .expect("the root remains routed")
                .bytes()
                .to_vec(),
            b"locally built app.html"
        );
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn a_new_incomplete_generation_is_never_admitted() {
        let root = worktree();
        staged(&root, &app());
        let source = AppSource::resolve(&root).expect("a complete stage resolves");
        let second = staged_as(&root, SECOND_GENERATION, &app()[..2]);

        let error = source
            .served("/")
            .expect_err("a newly published partial generation refuses");
        let AppSourceError::Incomplete {
            directory, missing, ..
        } = error
        else {
            panic!("a partial publication refused as {error:?}");
        };
        assert_eq!(directory, second);
        assert!(missing.iter().any(|file| file == "app_bg.wasm"));
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn a_page_on_a_generation_staged_before_the_current_inventory_moves_forward() {
        let root = worktree();
        let older = app()
            .into_iter()
            .filter(|(file, _)| *file != wire::manifest::WORK_MAP_STYLE)
            .collect::<Vec<_>>();
        staged(&root, &older);
        staged_as(&root, SECOND_GENERATION, &app());
        let canvas = crate::canvas::CanvasAssets::DevelopmentDirectory(
            AppSource::resolve(&root).expect("current generation is complete"),
        );

        // Neither the file it lacks nor one it holds is served from the older
        // generation: the whole page reloads into the served one.
        for file in [wire::manifest::APP_STYLE, wire::manifest::WORK_MAP_STYLE] {
            let answer = canvas
                .answer(&format!("{APP_GENERATION_ROUTE}/{FIRST_GENERATION}/{file}"))
                .expect("an older staged generation answers");
            assert!(
                matches!(
                    &answer,
                    crate::canvas::CanvasAnswer::Gone(served) if served.as_str() == SECOND_GENERATION
                ),
                "{file} answered {answer:?}"
            );
        }
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn concurrent_asset_handles_share_one_admitted_byte_allocation() {
        const CONCURRENT_REQUESTS: usize = 32;

        let root = worktree();
        let stage = staged(&root, &app());
        let source = Arc::new(AppSource::resolve(&root).expect("a complete stage resolves"));
        let generation = source
            .current_generation()
            .expect("current generation resolves");
        let route = Arc::new(
            generation.route(
                APP.asset("/app_bg.wasm")
                    .expect("the closed inventory has wasm"),
            ),
        );
        fs::remove_file(stage.join("app_bg.wasm"))
            .expect("the admitted file is no longer available for request-time reads");

        let barrier = Arc::new(Barrier::new(CONCURRENT_REQUESTS));
        let handles = (0..CONCURRENT_REQUESTS)
            .map(|_| {
                let source = Arc::clone(&source);
                let route = Arc::clone(&route);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    source
                        .served(&route)
                        .expect("the admitted generation serves without disk")
                        .expect("the generation-qualified route exists")
                })
            })
            .collect::<Vec<_>>();
        let assets = handles
            .into_iter()
            .map(|handle| handle.join().expect("request thread completes"))
            .collect::<Vec<_>>();
        let shared = assets[0].bytes().as_ptr();
        assert!(
            assets.iter().all(|asset| asset.bytes().as_ptr() == shared),
            "concurrent responses must share one large immutable allocation"
        );
        assert!(assets.iter().all(|asset| {
            asset.digest()
                == &wire::Digest::compute(wire::Domain::DistArtifact, b"locally built app_bg.wasm")
        }));
        drop(assets);
        let after = source
            .served(&route)
            .expect("a completed request retains only the admitted cache")
            .expect("the generation-qualified route remains present");
        assert_eq!(after.bytes().as_ptr(), shared);
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn the_generation_cache_is_bounded_and_pins_the_current_page() {
        let root = worktree();
        staged(&root, &app());
        let source = AppSource::resolve(&root).expect("a complete stage resolves");
        let first_route = source
            .current_generation()
            .expect("first generation resolves")
            .route(
                APP.asset("/app_bg.wasm")
                    .expect("the closed inventory has wasm"),
            );
        staged_as(&root, SECOND_GENERATION, &app());
        source.served("/").expect("second generation admits");
        staged_as(&root, THIRD_GENERATION, &app());
        source.served("/").expect("third generation admits");
        source
            .served(&first_route)
            .expect("an older stored generation can be read through the bounded cache");

        let cache = source
            .generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(cache.generations.len(), APP_GENERATION_CACHE_CAPACITY);
        let first = wire::Digest::from_str(FIRST_GENERATION).expect("canonical generation");
        let second = wire::Digest::from_str(SECOND_GENERATION).expect("canonical generation");
        let third = wire::Digest::from_str(THIRD_GENERATION).expect("canonical generation");
        assert!(cache.generations.contains_key(&first));
        assert!(!cache.generations.contains_key(&second));
        assert!(cache.generations.contains_key(&third));
        assert_eq!(cache.current.as_ref(), Some(&third));
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn discovery_is_atomic_owner_only_and_never_debugs_its_capability() {
        let root = std::env::temp_dir().join(format!(
            "spice-serve-discovery-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("fixture directory");
        let path = root.join("serve");
        let discovery = Discovery {
            endpoint: "http://127.0.0.1:48123".to_owned(),
            capability: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
            epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c4"),
        };

        discovery.publish(&path).expect("discovery publishes");
        let decoded: Discovery = wire::decode(&fs::read(&path).expect("published bytes"))
            .expect("published discovery decodes");

        assert_eq!(decoded, discovery);
        assert!(!format!("{discovery:?}").contains(&discovery.capability.to_string()));
        assert_eq!(
            sill::authority::owner_mode(&path).expect("published mode"),
            0o600
        );
        fs::remove_dir_all(root).expect("fixture cleanup");
    }
}
