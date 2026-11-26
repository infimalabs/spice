//! The browser app canvas: the `app` directory of the spice rack and the
//! boundary a resident serves app routes through.

mod assets;
pub mod gzip;

/// Rack directory holding the browser app generation.
pub const APP_DIRECTORY: &str = "app";

/// The build record at the rack root: a [`wire::manifest::BuildInfo`] JSON
/// document naming the product version and source commit of the build that
/// linked the rack.
pub const BUILD: &str = "build.json";

/// Extension of the app rack staged beside each generation directory:
/// `generations/G.zip` holds generation G's `app` directory.
pub const ARCHIVE_EXTENSION: &str = "zip";

/// The shell attribute naming the generation its routes are qualified with.
const SHELL_GENERATION: &str = "data-app-generation=\"";

pub(crate) use assets::CanvasAnswer;
pub use assets::{BUILD_ROUTE, CanvasAssets, EmbeddedArchive, RTL_SHELL_ROUTE};

/// The app generation a rack carries: the one its shell qualifies routes with.
///
/// # Errors
///
/// Refuses a rack without an app shell, a shell that does not inflate, and a
/// shell that names no generation, more than one, or one that is not a digest.
pub fn generation(rack: &rack::Rack<'_>) -> Result<wire::Digest, rack::Error> {
    let path = shell_path();
    let shell = gzip::get(rack, &path)?
        .ok_or_else(|| shell_error(&path, "is absent"))?
        .inflate()?;
    shell_generation(&path, &shell)
}

fn shell_path() -> String {
    format!("{APP_DIRECTORY}/{}", wire::manifest::APP_HTML)
}

fn shell_generation(path: &str, shell: &[u8]) -> Result<wire::Digest, rack::Error> {
    let html = std::str::from_utf8(shell).map_err(|_| shell_error(path, "is not UTF-8"))?;
    let mut spans = html.split(SHELL_GENERATION).skip(1);
    let value = spans
        .next()
        .ok_or_else(|| shell_error(path, "names no generation"))?;
    if spans.next().is_some() {
        return Err(shell_error(path, "names more than one generation"));
    }
    value
        .split_once('"')
        .and_then(|(generation, _)| generation.parse().ok())
        .ok_or_else(|| shell_error(path, "names a generation that is not a digest"))
}

fn shell_error(path: &str, reason: &'static str) -> rack::Error {
    rack::Error::Member {
        path: path.to_owned(),
        reason,
    }
}
