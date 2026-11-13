//! Reads of this executable's spice rack: one member's text, a pointer line
//! handing off a non-text member's bytes, or the members beneath a directory
//! as a `resources:` footer.
//!
//! A rack URL may name a generation by the short code of the journal fact
//! that admitted the linked rack; this executable serves only that one.

use std::fs;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Request, Resource, SCHEME, Selector, push_resources};
use rack::Rack;
use serve::canvas::EmbeddedArchive;
use serve::canvas::gzip::{self, Stream};
use spice::repository::StatePaths;
use wire::intent::{
    RACK_DIRECTORY_ENTRIES_MAX, RACK_PATH_BYTES_MAX, RACK_TEXT_BYTES_MAX, RackAnswer, RackContent,
    RackEntry, RackEntryKind, RackIdentity, RackRead,
};
use wire::{CanonicalUuid, Digest, Domain, Media, SequenceCode};

/// What a rack read takes from the runtime directory: where it extracts and
/// the short code of the fact admitting the linked rack, if one was appended.
pub(super) struct Runtime {
    pub(super) extractions: PathBuf,
    pub(super) code: Option<SequenceCode>,
}

/// Whether the linked rack holds this path as a member or a directory; fixed
/// reads never need to load it.
pub(super) fn linked_member(path: &str, url: &str) -> Result<bool, String> {
    Ok(linked_archive(url)?.is_some_and(|linked| holds(linked.rack(), path)))
}

/// Whether `path` names a member of `rack` or a directory holding members.
pub(super) fn holds(rack: &Rack<'_>, path: &str) -> bool {
    rack.get(&gzip::stored_name(path)).is_some() || rack.beneath(path).next().is_some()
}

fn linked_archive(url: &str) -> Result<Option<&'static EmbeddedArchive>, String> {
    match spice::version::canvas() {
        Some(Ok(linked)) => Ok(Some(linked)),
        Some(Err(error)) => Err(format!(
            "{url} names a rack member, but this executable's spice rack is refused: {error}"
        )),
        None => Ok(None),
    }
}

impl Request {
    /// A rack member or directory read with no query. A generation may qualify
    /// the first segment or trail the member path. Unknown paths return none.
    pub(super) fn rack(
        mut path: String,
        query: &str,
        selector: Option<Selector>,
        url: &str,
        member: impl FnOnce(&str) -> Result<bool, String>,
    ) -> Result<Option<Self>, String> {
        let first_end = path.find('/').unwrap_or(path.len());
        let generation = match path[..first_end].rfind('@').or_else(|| path.rfind('@')) {
            Some(start) => {
                let end = if start < first_end {
                    first_end
                } else {
                    path.len()
                };
                let code = generation_code(url, &path[start + 1..end])?;
                path.replace_range(start..end, "");
                Some(code)
            }
            None => None,
        };
        if !member(&path)? {
            return Ok(None);
        }
        if !query.is_empty() {
            return Err(format!(
                "{url} takes no query; use a trailing line selector to bound the read"
            ));
        }
        Ok(Some(Self {
            resource: Resource::Rack { path, generation },
            selector,
        }))
    }
}

/// The short code a rack URL's `@` segment names: a digest or UUID never
/// names a generation, and other text is no code.
fn generation_code(url: &str, generation: &str) -> Result<SequenceCode, String> {
    if generation.parse::<Digest>().is_ok() || generation.parse::<CanonicalUuid>().is_ok() {
        return Err(format!(
            "{url} names a generation by digest or UUID; a rack URL names a generation only by \
             the journal short code of the fact that admitted it"
        ));
    }
    generation
        .parse()
        .map_err(|_| format!("{url} names generation {generation}, which is no journal short code"))
}

/// Why a rack URL naming `generation` is refused: this executable serves
/// only the rack it links, admitted as `linked` or by no fact at all.
fn generation_refusal(url: &str, generation: SequenceCode, linked: Option<SequenceCode>) -> String {
    match linked {
        Some(linked) => format!(
            "{url} names generation {generation}, but this executable links generation {linked}; \
             read @{linked} or drop the segment"
        ),
        None => format!(
            "{url} names generation {generation}, but no journal fact admits the rack this \
             executable links; drop @{generation}"
        ),
    }
}

/// [`document`] against the rack linked into this executable, reading the
/// runtime directory `state` admits for its extractions and journal.
pub(super) fn linked(
    path: &str,
    generation: Option<SequenceCode>,
    url: &str,
    state: impl FnOnce() -> Result<StatePaths, String>,
) -> Result<String, String> {
    let linked = linked_archive(url)?.ok_or_else(|| {
        format!("{url} names a rack member, but this executable links no spice rack")
    })?;
    document(linked.rack(), path, generation, url, || {
        let state = state()?;
        let archive =
            spice::version::rack_archive().expect("an admitted linked rack has archive bytes");
        Ok(Runtime {
            extractions: state.rack().join(linked.generation().as_str()),
            code: admission_code(&state, archive, url)?,
        })
    })
}
/// The linked rack and the archive identity its admission fact names, for an
/// app read whose caller resolves that fact's code from its own journal read.
pub(crate) fn linked_for_app() -> Result<(&'static EmbeddedArchive, &'static Digest), String> {
    let linked = linked_archive("rack::read")?.ok_or_else(|| {
        "rack::read is unavailable because this executable links no rack".to_owned()
    })?;
    let archive = spice::version::rack_archive().ok_or_else(|| {
        "rack::read is unavailable because this executable links no rack archive".to_owned()
    })?;
    Ok((linked, archive))
}

/// Read one app document from an already admitted rack.
pub(crate) fn read_app_document(
    rack: &Rack<'_>,
    archive: Digest,
    generation: Option<SequenceCode>,
    request: &RackRead,
) -> Result<RackAnswer, String> {
    let path = app_path(&request.path)?;
    if let Some(expected) = request
        .generation
        .filter(|expected| Some(*expected) != generation)
    {
        return Err(match generation {
            Some(actual) => format!(
                "rack generation {expected} was requested, but this resident links generation {actual}"
            ),
            None => format!(
                "rack generation {expected} was requested, but no journal fact admits this resident's linked rack"
            ),
        });
    }
    let identity = RackIdentity {
        archive,
        generation,
    };
    if let Some(stream) = gzip::get(rack, &path).map_err(|error| error.to_string())? {
        return read_app_member(identity, request.path.clone(), stream);
    }
    read_app_directory(rack, identity, &path, &request.path)
}

/// Map one normalized docs-relative request path onto the linked rack.
fn app_path(relative: &str) -> Result<String, String> {
    if relative.len() > RACK_PATH_BYTES_MAX {
        return Err(format!(
            "rack path is {} bytes; the maximum is {RACK_PATH_BYTES_MAX}",
            relative.len()
        ));
    }
    if relative.starts_with('/')
        || relative.contains('\\')
        || relative.chars().any(char::is_control)
        || relative
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        if relative.is_empty() {
            return Ok("docs".to_owned());
        }
        return Err(format!(
            "rack path {relative:?} is not a normalized path relative to docs"
        ));
    }
    Ok(format!("docs/{relative}"))
}

fn read_app_directory(
    rack: &Rack<'_>,
    identity: RackIdentity,
    path: &str,
    relative: &str,
) -> Result<RackAnswer, String> {
    let mut entries = Vec::new();
    let mut previous_directory = None::<&str>;
    for entry in gzip::beneath(rack, path) {
        let (file, stream) = entry.map_err(|error| error.to_string())?;
        let (name, directory) = file
            .split_once('/')
            .map_or((file, false), |(name, _)| (name, true));
        if directory && previous_directory == Some(name) {
            continue;
        }
        if entries.len() == RACK_DIRECTORY_ENTRIES_MAX {
            return Err(format!(
                "rack directory {relative:?} has more than {RACK_DIRECTORY_ENTRIES_MAX} immediate children"
            ));
        }
        let child = if relative.is_empty() {
            name.to_owned()
        } else {
            format!("{relative}/{name}")
        };
        if directory {
            entries.push(RackEntry {
                path: child,
                kind: RackEntryKind::Directory,
                content_type: None,
                bytes: None,
            });
            previous_directory = Some(name);
            continue;
        }
        previous_directory = None;
        let content_type = serve::content_type(stream.path());
        entries.push(RackEntry {
            path: child,
            kind: app_entry_kind(content_type),
            content_type: content_type.map(str::to_owned),
            bytes: Some(u64::from(stream.size())),
        });
    }
    if entries.is_empty() {
        return Err(format!(
            "rack path {relative:?} names no member beneath docs"
        ));
    }
    Ok(RackAnswer {
        identity,
        path: relative.to_owned(),
        content: RackContent::Directory { entries },
    })
}

fn read_app_member(
    identity: RackIdentity,
    relative: String,
    stream: Stream<'_>,
) -> Result<RackAnswer, String> {
    let content_type = content_type(stream.path(), "rack::read")?;
    let essence = content_type_essence(content_type);
    let content = if matches!(essence, "text/markdown" | "text/plain") {
        if stream.size() as usize > RACK_TEXT_BYTES_MAX {
            return Err(format!(
                "rack text member {relative:?} is {} bytes; the maximum is {RACK_TEXT_BYTES_MAX}",
                stream.size()
            ));
        }
        let bytes = stream.inflate().map_err(|error| error.to_string())?;
        let text = String::from_utf8(bytes).map_err(|_| {
            format!("rack member {relative:?} declares {content_type} but is not UTF-8")
        })?;
        RackContent::Text {
            content_type: content_type.to_owned(),
            text,
        }
    } else if wire::media::supports_content_type(essence) {
        if stream.size() as usize > wire::media::MAX_MEDIA_BYTES {
            return Err(format!(
                "rack image member {relative:?} is {} bytes; the maximum is {}",
                stream.size(),
                wire::media::MAX_MEDIA_BYTES
            ));
        }
        let bytes = stream.inflate().map_err(|error| error.to_string())?;
        let name = stream
            .path()
            .rsplit('/')
            .next()
            .expect("a normalized rack path has a final component");
        let media = Media::from_bytes(name, essence, &bytes)
            .map_err(|error| format!("rack image member {relative:?} is refused: {error}"))?;
        if media.data_url.len() > wire::media::MAX_MEDIA_DATA_URL_BYTES {
            return Err(format!(
                "rack image member {relative:?} encodes to {} bytes; the maximum is {}",
                media.data_url.len(),
                wire::media::MAX_MEDIA_DATA_URL_BYTES
            ));
        }
        RackContent::Image { media }
    } else {
        return Err(format!(
            "rack member {relative:?} declares unsupported content type {content_type}"
        ));
    };
    Ok(RackAnswer {
        identity,
        path: relative,
        content,
    })
}

fn app_entry_kind(content_type: Option<&str>) -> RackEntryKind {
    match content_type.map(content_type_essence) {
        Some("text/markdown" | "text/plain") => RackEntryKind::Document,
        Some(content_type) if wire::media::supports_content_type(content_type) => {
            RackEntryKind::Image
        }
        _ => RackEntryKind::Unsupported,
    }
}

fn content_type_essence(content_type: &str) -> &str {
    content_type
        .split_once(';')
        .map_or(content_type, |(essence, _)| essence)
        .trim()
}

/// The short code of the fact admitting `archive`: none while the repository
/// has no journal or its journal names no such fact.
fn admission_code(
    state: &StatePaths,
    archive: &Digest,
    url: &str,
) -> Result<Option<SequenceCode>, String> {
    let journal = state.journal();
    let unreadable = |error: &dyn std::fmt::Display| {
        format!("{url} names a generation the journal could not answer: {error}")
    };
    match journal.try_exists() {
        Ok(true) => {}
        Ok(false) => return Ok(None),
        Err(error) => return Err(unreadable(&error)),
    }
    let reader = journal::JournalReader::open(journal).map_err(|error| unreadable(&error))?;
    reader
        .read_with(|read| spice::rack_admission::code(read, archive))
        .map_err(|error| unreadable(&error))
}

/// The text of one rack member, the pointer line handing off a non-text
/// member extracted beneath the runtime's extractions, or, when `path` names
/// a directory, its members as a `resources:` footer.
///
/// The runtime is resolved only when the read needs it: to check the
/// generation a URL names, or to hand off bytes under the linked code.
pub(super) fn document(
    rack: &Rack<'_>,
    path: &str,
    generation: Option<SequenceCode>,
    url: &str,
    runtime: impl FnOnce() -> Result<Runtime, String>,
) -> Result<String, String> {
    let stored = gzip::get(rack, path)
        .map_err(|error| error.to_string())?
        .map(|stream| Ok::<_, String>((stream, content_type(path, url)?)))
        .transpose()?;
    let handed_off = stored.filter(|(_, content_type)| !is_text(content_type));
    if generation.is_none() && handed_off.is_none() {
        return text_or_listing(rack, path, stored, url);
    }
    let runtime = runtime()?;
    if let Some(generation) = generation.filter(|generation| runtime.code != Some(*generation)) {
        return Err(generation_refusal(url, generation, runtime.code));
    }
    match handed_off {
        Some((stream, content_type)) => handoff(
            stream,
            content_type,
            &runtime.extractions,
            runtime.code,
            url,
        ),
        None => text_or_listing(rack, path, stored, url),
    }
}

/// The content type Spice's one table gives a member's path, refusing an
/// extension the table does not name.
fn content_type(path: &str, url: &str) -> Result<&'static str, String> {
    serve::content_type(path)
        .ok_or_else(|| format!("{url} names a member whose extension declares no content type"))
}

/// Whether bytes of `content_type` read inline as text rather than being
/// handed off as a file: its media type essence, before any parameter, is
/// textual.
fn is_text(content_type: &str) -> bool {
    let essence = content_type_essence(content_type);
    essence.starts_with("text/") || matches!(essence, "application/json" | "application/toml")
}

/// A text member's bytes as UTF-8, or the `resources:` footer beneath `path`.
fn text_or_listing(
    rack: &Rack<'_>,
    path: &str,
    stored: Option<(Stream<'_>, &str)>,
    url: &str,
) -> Result<String, String> {
    if let Some((stream, content_type)) = stored {
        let bytes = stream.inflate().map_err(|error| error.to_string())?;
        return String::from_utf8(bytes)
            .map_err(|_| format!("{url} declares {content_type} but is not UTF-8 text"));
    }
    let separator = if path.is_empty() { "" } else { "/" };
    let mut children = Vec::new();
    for entry in gzip::beneath(rack, path) {
        let (file, _) = entry.map_err(|error| error.to_string())?;
        if !path.is_empty() || !file.contains('/') {
            children.push(format!("{SCHEME}{path}{separator}{file}"));
        }
    }
    if children.is_empty() {
        return Err(format!(
            "{url} names no member of this executable's spice rack"
        ));
    }
    let mut document = format!(
        "{SCHEME}{path}{separator} spice rack members: {}",
        children.len()
    );
    push_resources(&mut document, &children);
    Ok(document)
}

/// One line naming a non-text member's media type, size, digest, the
/// generation short code when a fact admitted the rack, and the file in
/// `directory` holding its bytes. The file is named by the bytes' digest and
/// written once: a later read finds it and writes nothing.
fn handoff(
    stored: Stream<'_>,
    content_type: &str,
    directory: &Path,
    code: Option<SequenceCode>,
    url: &str,
) -> Result<String, String> {
    let size = stored.size();
    let bytes = stored.inflate().map_err(|error| error.to_string())?;
    let digest = Digest::compute(Domain::DistArtifact, &bytes);
    let name = match stored
        .path()
        .rsplit('/')
        .next()
        .and_then(|file| file.rsplit_once('.'))
    {
        Some((_, extension)) => format!("{digest}.{extension}"),
        None => digest.to_string(),
    };
    let file = directory.join(name);
    let refused = |error: std::io::Error| {
        format!(
            "{url} could not be extracted to {}: {error}",
            file.display()
        )
    };
    match fs::symlink_metadata(&file) {
        Ok(metadata) if metadata.is_file() && metadata.len() == u64::from(size) => {}
        Ok(_) => {
            return Err(refused(std::io::Error::new(
                ErrorKind::AlreadyExists,
                "another entry holds the digest's name",
            )));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            extract(directory, &file, &bytes).map_err(refused)?;
        }
        Err(error) => return Err(refused(error)),
    }
    let generation = code.map_or_else(String::new, |code| format!(" generation {code}"));
    Ok(format!(
        "{content_type} {size} bytes digest {digest}{generation} at {}",
        file.display()
    ))
}

/// Write `bytes` to `file` through a private sibling renamed into place, so
/// the digest-named file is never seen partial and never rewritten.
fn extract(directory: &Path, file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static PARTIALS: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(directory)?;
    let partial = directory.join(format!(
        ".partial-{}-{}",
        std::process::id(),
        PARTIALS.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)?;
        output.write_all(bytes)?;
        output.sync_all()
    })()
    .and_then(|()| fs::rename(&partial, file));
    match written {
        Ok(()) => Ok(()),
        // A racing read renamed the same digest's bytes into place first.
        Err(_) if fs::symlink_metadata(file).is_ok_and(|metadata| metadata.is_file()) => {
            let _ = fs::remove_file(&partial);
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&partial);
            Err(error)
        }
    }
}
