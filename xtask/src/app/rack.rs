//! The spice rack every appliance build links: the staged app generation, the
//! tracked sources beside it, and the build record, assembled in one place.
//!
//! Release assembly and a development refresh both link what [`assemble`]
//! answers, so the same staged generation, the same tracked sources, and the
//! same build give byte-identical racks: members follow Spice's
//! [`gzip`] convention, and no timestamp or host path enters the archive.

use std::path::{Component, Path};

use rack::Rack;
use spice::serve::canvas::gzip;
use spice::serve::canvas::{APP_DIRECTORY, BUILD};
use wire::manifest::BuildInfo;

use super::AppBuildError;

/// The agent skill, at the rack root.
pub(super) const SKILL: &str = "SKILL.md";
/// The packaged configuration defaults, at the rack root.
pub(super) const POLICY: &str = "spice.toml";
/// The product license, stored with a plain-text extension for HTTP serving.
pub(super) const LICENSE: &str = "LICENSE.txt";
/// The documentation directory.
pub(super) const DOCS: &str = "docs";

/// One tracked worktree source of the rack besides the app directory, which
/// the staged generation supplies.
enum Tracked {
    /// One regular file stored under an explicit rack member path.
    File {
        path: &'static str,
        member: &'static str,
    },
    /// Every regular file beneath a worktree directory, stored at its path
    /// within it beneath a rack directory.
    Directory {
        path: &'static str,
        directory: &'static str,
    },
}

/// The tracked source list.
const TRACKED: &[Tracked] = &[
    Tracked::File {
        path: "spice/agent/SKILL.md",
        member: SKILL,
    },
    Tracked::File {
        path: "gate/defaults/spice.toml",
        member: POLICY,
    },
    Tracked::File {
        path: "LICENSE",
        member: LICENSE,
    },
    Tracked::Directory {
        path: "docs",
        directory: DOCS,
    },
];

/// Assemble the rack from the admitted app archive staged for its
/// generation, the tracked sources under `root`, and `build`, stored as the
/// root [`BUILD`] member.
///
/// App members keep their stored gzip streams, so only the tracked sources
/// and the build record are deflated here. Every member's extension must be
/// one Spice's content-type table names.
pub(super) fn assemble(
    root: &Path,
    app: &Rack<'_>,
    build: &BuildInfo,
) -> Result<Vec<u8>, AppBuildError> {
    let mut tracked = vec![(BUILD.to_owned(), dist::build_record_bytes(build))];
    for source in TRACKED {
        match source {
            Tracked::File { path, member } => {
                tracked.push(((*member).to_owned(), super::read_regular(&root.join(path))?));
            }
            Tracked::Directory { path, directory } => {
                let source = root.join(path);
                let mut files = Vec::new();
                super::source::collect_directory(root, &source, &mut files)?;
                for file in files {
                    let absolute = root.join(&file);
                    let member = member_path(&absolute, &source)?;
                    tracked.push((
                        format!("{directory}/{member}"),
                        super::read_regular(&absolute)?,
                    ));
                }
            }
        }
    }
    for entry in gzip::beneath(app, APP_DIRECTORY) {
        let (_, stream) = entry.map_err(AppBuildError::Canvas)?;
        super::typed(stream.path())?;
    }
    let compressed = tracked
        .iter()
        .map(|(path, bytes)| {
            super::typed(path)?;
            Ok((gzip::stored_name(path), gzip::compress(bytes)))
        })
        .collect::<Result<Vec<_>, AppBuildError>>()?;
    rack::write(
        app.beneath(APP_DIRECTORY)
            .map(|(_, member)| (member.path(), member.bytes()))
            .chain(
                compressed
                    .iter()
                    .map(|(name, stream)| (name.as_str(), stream.as_slice())),
            ),
    )
    .map_err(AppBuildError::Canvas)
}

/// `file`'s path below `directory`, its segments joined by `/` on every host.
fn member_path(file: &Path, directory: &Path) -> Result<String, AppBuildError> {
    let relative = file.strip_prefix(directory).map_err(|_| {
        AppBuildError::SourceIdentity(format!(
            "{} escaped {} while rack sources were collected",
            file.display(),
            directory.display()
        ))
    })?;
    relative
        .components()
        .map(|component| match component {
            Component::Normal(segment) => segment
                .to_str()
                .ok_or_else(|| AppBuildError::NonUtf8(file.to_path_buf())),
            _ => Err(AppBuildError::SourceIdentity(format!(
                "{} is not a plain path below {}",
                file.display(),
                directory.display()
            ))),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|segments| segments.join("/"))
}
