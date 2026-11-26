//! The one boundary the resident serves browser app routes through.
//!
//! A running resident answers from exactly one of two sources: the staged
//! generation directory of a staged `spice up`, which hot-swaps as new
//! generations are published, or the admitted rack an installed
//! executable carries, which holds one generation for the process lifetime.
//! A route qualified with a generation the source does not hold, or with one
//! staged before the current inventory, is gone; the page answers that by
//! reloading once into the generation now served.

use std::collections::BTreeMap;
use std::sync::Arc;

use ::axum::body::Bytes;
use ::axum::http::HeaderValue;

use crate::asset::{AppSource, AppSourceError, generation_route};

use super::BUILD;

/// The browser app source a resident serves.
#[derive(Clone, Debug)]
pub enum CanvasAssets {
    /// The staged generation directory with its hot swap and two-entry
    /// generation cache, selected only for a staged `spice up`.
    DevelopmentDirectory(AppSource),
    /// The admitted archive linked into an installed executable; it reads no
    /// app file from disk.
    EmbeddedArchive(EmbeddedArchive),
}

/// What one app route answers.
#[derive(Debug)]
pub(crate) enum CanvasAnswer {
    Asset(CanvasAsset),
    /// The route names a generation this source no longer holds; it carries
    /// the generation served now.
    Gone(wire::Digest),
    NotFound,
}

/// One response body with the metadata its headers need.
#[derive(Debug)]
pub(crate) struct CanvasAsset {
    pub(crate) body: Bytes,
    pub(crate) content_type: &'static str,
    pub(crate) etag: HeaderValue,
    /// The body is one stored gzip stream, served as `Content-Encoding: gzip`.
    pub(crate) gzip: bool,
    /// The inflated HTML document a content security policy is derived from.
    pub(crate) document: Option<Arc<[u8]>>,
}

impl CanvasAsset {
    /// This shell document with `dir="rtl"` on its root element, under its
    /// own ETag; `None` when the document carries no root element.
    pub(crate) fn right_to_left(self) -> Option<Self> {
        const ROOT: &str = "<html";
        let document = self.document?;
        let html = std::str::from_utf8(&document).ok()?;
        let at = html.find(ROOT)? + ROOT.len();
        let directed: Arc<[u8]> =
            Arc::from(format!("{} dir=\"rtl\"{}", &html[..at], &html[at..]).into_bytes());
        let etag = HeaderValue::from_str(&format!(
            "{}-rtl\"",
            self.etag.to_str().ok()?.strip_suffix('"')?
        ))
        .ok()?;
        Some(Self {
            body: Bytes::from_owner(Arc::clone(&directed)),
            content_type: self.content_type,
            etag,
            gzip: false,
            document: Some(directed),
        })
    }
}

impl CanvasAssets {
    /// The generation the shell route serves now.
    ///
    /// # Errors
    ///
    /// Returns the staged directory's pointer, inventory, or read failure.
    pub fn generation(&self) -> Result<wire::Digest, AppSourceError> {
        match self {
            Self::DevelopmentDirectory(source) => {
                Ok(source.current_generation()?.identity().clone())
            }
            Self::EmbeddedArchive(archive) => Ok(archive.0.generation.clone()),
        }
    }

    /// Answer one exact route. Unknown paths have no shell fallback.
    pub(crate) fn answer(&self, route: &str) -> Result<CanvasAnswer, AppSourceError> {
        match self {
            Self::DevelopmentDirectory(source) => {
                if let Some((identity, _)) = generation_route(route)
                    && !source.holds(&identity)
                {
                    return Ok(CanvasAnswer::Gone(
                        source.current_generation()?.identity().clone(),
                    ));
                }
                let served = match source.served(route) {
                    // An older staged generation lacking a current inventory
                    // file is never served partially; its page moves forward.
                    Err(AppSourceError::Incomplete { .. }) if generation_route(route).is_some() => {
                        return Ok(CanvasAnswer::Gone(
                            source.current_generation()?.identity().clone(),
                        ));
                    }
                    served => served?,
                };
                Ok(served.map_or(CanvasAnswer::NotFound, |asset| {
                    let etag = HeaderValue::from_str(&format!("\"{}\"", asset.digest()))
                        .expect("a canonical digest is an admitted ETag");
                    let content_type = asset.content_type();
                    let bytes = asset.into_shared_bytes();
                    CanvasAnswer::Asset(CanvasAsset {
                        document: is_document(content_type).then(|| Arc::clone(&bytes)),
                        body: Bytes::from_owner(bytes),
                        content_type,
                        etag,
                        gzip: false,
                    })
                }))
            }
            Self::EmbeddedArchive(archive) => Ok(archive.answer(route)),
        }
    }
}

impl std::fmt::Display for CanvasAssets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DevelopmentDirectory(source) => source.fmt(formatter),
            Self::EmbeddedArchive(archive) => {
                write!(formatter, "embedded app {}", archive.0.generation)
            }
        }
    }
}

/// The HTTP route answering the linked rack's build record.
pub const BUILD_ROUTE: &str = "/build.json";

/// The current shell with `dir="rtl"` on its root element, loading the same
/// generation's assets, so layout geometry can be read right to left.
pub const RTL_SHELL_ROUTE: &str = "/rtl/";

/// The admitted rack of an installed executable, its app members indexed once.
#[derive(Clone)]
pub struct EmbeddedArchive(Arc<Embedded>);

struct Embedded {
    generation: wire::Digest,
    build: wire::manifest::BuildInfo,
    /// The stored build record, served at [`BUILD_ROUTE`].
    record: EmbeddedMember,
    members: BTreeMap<&'static str, EmbeddedMember>,
    rack: rack::Rack<'static>,
}

struct EmbeddedMember {
    gzip: &'static [u8],
    content_type: &'static str,
    etag: HeaderValue,
    document: Option<Arc<[u8]>>,
}

impl EmbeddedArchive {
    /// Admit the executable's rack and index every app member once: its
    /// stored gzip slice, its content type, and its strong ETag. HTML members
    /// are inflated here, once, for their content security policies; the
    /// shell among them names the generation.
    ///
    /// # Errors
    ///
    /// Refuses everything [`rack::Rack::admit`] refuses, a rack without a
    /// canonical [`wire::manifest::BuildInfo`] record, a member the
    /// [`super::gzip`] convention or the [`crate::content_type`] table refuses,
    /// an HTML member that is not exactly one gzip stream, and a shell
    /// [`super::generation`] refuses.
    pub fn admit(bytes: &'static [u8]) -> Result<Self, rack::Error> {
        let rack = rack::Rack::admit(bytes)?;
        let build_error = |reason| rack::Error::Member {
            path: BUILD.to_owned(),
            reason,
        };
        let stored = super::gzip::get(&rack, BUILD)?
            .ok_or_else(|| build_error("is absent from the linked rack"))?;
        let build: wire::manifest::BuildInfo = serde_json::from_slice(&stored.inflate()?)
            .map_err(|_| build_error("is not a build record"))?;
        if build.source.as_ref().is_some_and(|source| {
            !wire::manifest::git_commit_is_canonical(&source.commit) || source.committed < 0
        }) {
            return Err(build_error("does not name one canonical source identity"));
        }
        // The record's own stored bytes name its ETag: a rebuilt record under
        // an unchanged app generation is never answered as unmodified.
        let record = EmbeddedMember {
            gzip: stored.gzip(),
            content_type: content_type(BUILD)?,
            etag: HeaderValue::from_str(&format!(
                "\"{}\"",
                wire::Digest::compute(wire::Domain::DistArtifact, stored.gzip())
            ))
            .expect("a canonical digest is an admitted ETag"),
            document: None,
        };
        let indexed = super::gzip::beneath(&rack, super::APP_DIRECTORY)
            .map(|entry| {
                let (path, stream) = entry?;
                let content_type = content_type(stream.path())?;
                let document = if is_document(content_type) {
                    Some(Arc::<[u8]>::from(stream.inflate()?))
                } else {
                    None
                };
                Ok((path, stream.gzip(), content_type, document))
            })
            .collect::<Result<Vec<_>, rack::Error>>()?;
        let shell_path = super::shell_path();
        let shell = indexed
            .iter()
            .find(|(path, ..)| *path == wire::manifest::APP_HTML)
            .and_then(|(.., document)| document.as_deref())
            .ok_or_else(|| super::shell_error(&shell_path, "is absent"))?;
        let generation = super::shell_generation(&shell_path, shell)?;
        let members = indexed
            .into_iter()
            .enumerate()
            .map(|(index, (path, gzip, content_type, document))| {
                let etag = HeaderValue::from_str(&format!("\"{generation}/{index}\""))
                    .expect("a digest and an index are an admitted ETag");
                (
                    path,
                    EmbeddedMember {
                        gzip,
                        content_type,
                        etag,
                        document,
                    },
                )
            })
            .collect();
        Ok(Self(Arc::new(Embedded {
            generation,
            build,
            record,
            members,
            rack,
        })))
    }

    /// The whole admitted rack, not only the app it serves.
    #[must_use]
    pub fn rack(&self) -> &rack::Rack<'static> {
        &self.0.rack
    }

    /// The immutable generation this embedded source serves.
    #[must_use]
    pub fn generation(&self) -> &wire::Digest {
        &self.0.generation
    }

    /// The build that linked this rack: its product version and source.
    #[must_use]
    pub fn build(&self) -> &wire::manifest::BuildInfo {
        &self.0.build
    }

    fn answer(&self, route: &str) -> CanvasAnswer {
        if route == BUILD_ROUTE {
            return CanvasAnswer::Asset(self.0.record.asset());
        }
        let path = if route == "/" {
            wire::manifest::APP_HTML
        } else {
            let Some((identity, file)) = generation_route(route) else {
                return CanvasAnswer::NotFound;
            };
            if identity != self.0.generation {
                return CanvasAnswer::Gone(self.0.generation.clone());
            }
            file
        };
        self.0
            .members
            .get(path)
            .map_or(CanvasAnswer::NotFound, |member| {
                CanvasAnswer::Asset(member.asset())
            })
    }
}

impl EmbeddedMember {
    fn asset(&self) -> CanvasAsset {
        CanvasAsset {
            body: Bytes::from_static(self.gzip),
            content_type: self.content_type,
            etag: self.etag.clone(),
            gzip: true,
            document: self.document.clone(),
        }
    }
}

impl std::fmt::Debug for EmbeddedArchive {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddedArchive")
            .field("generation", &self.0.generation)
            .field("members", &self.0.members.len())
            .finish()
    }
}

/// The table's content type for one rack member, refusing an extension it
/// does not name.
fn content_type(path: &str) -> Result<&'static str, rack::Error> {
    crate::content_type(path).ok_or_else(|| rack::Error::Member {
        path: path.to_owned(),
        reason: "extension names no content type",
    })
}

fn is_document(content_type: &str) -> bool {
    content_type == crate::asset::DOCUMENT_CONTENT_TYPE
}
