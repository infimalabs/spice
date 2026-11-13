//! Building a deterministic rack from entries.

use std::io::{Cursor, Write as _};

use zip::write::SimpleFileOptions;

use super::{Error, check_path, member_error};

/// Write the deterministic rack of `entries`, each a member's path and exact
/// bytes.
///
/// Members are ordered by path bytes whatever the input order, and every
/// record is STORE at the fixed DOS epoch, so the same entries always yield
/// the same bytes.
///
/// # Errors
///
/// Refuses a repeated path or one that is not a normalized relative path, and
/// entries the ZIP writer cannot represent.
pub fn write<'a>(entries: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Result<Vec<u8>, Error> {
    let mut entries = entries.into_iter().collect::<Vec<_>>();
    entries.sort_unstable_by_key(|(path, _)| *path);
    if let Some(pair) = entries.windows(2).find(|pair| pair[0].0 == pair[1].0) {
        return Err(member_error(pair[0].0, "path is repeated"));
    }
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in entries {
        check_path(path)?;
        archive.start_file(path, options)?;
        archive
            .write_all(bytes)
            .map_err(|error| Error::Zip(error.to_string()))?;
    }
    Ok(archive.finish()?.into_inner())
}
