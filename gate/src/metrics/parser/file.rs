use super::{ParseError, canonical_path, python_splitlines};

/// Native line/byte counts for one source file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileShapeMeasurement {
    path: String,
    lines: u64,
    bytes: u64,
}

impl FileShapeMeasurement {
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub const fn lines(&self) -> u64 {
        self.lines
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// Native decoded-character count for one repository-truth document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryDocumentMeasurement {
    path: String,
    characters: u64,
}

impl RepositoryDocumentMeasurement {
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub const fn characters(&self) -> u64 {
        self.characters
    }
}

/// Measure source lines, split at every Unicode line boundary, and exact bytes.
///
/// # Errors
///
/// NUL-bearing binary inputs and invalid UTF-8 are explicit unsupported
/// states: a source file is UTF-8 text or it is not measured.
pub fn measure_file_shape(path: &str, bytes: &[u8]) -> Result<FileShapeMeasurement, ParseError> {
    let path = canonical_path(path)?;
    if bytes.contains(&0) {
        return Err(ParseError::unsupported(
            "policy.file_shape.source_suffixes",
            format!("selected source {path:?} contains a NUL byte"),
            "remove the path from the source file class or replace it with UTF-8 text",
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|error| {
        ParseError::unsupported(
            "policy.file_shape.source_suffixes",
            format!("selected source {path:?} is not UTF-8: {error}"),
            "remove the path from the source file class or encode it as UTF-8",
        )
    })?;
    Ok(FileShapeMeasurement {
        path,
        lines: u64::try_from(python_splitlines(text).len()).expect("line count fits in u64"),
        bytes: u64::try_from(bytes.len()).expect("file byte length fits in u64"),
    })
}

/// Measure the decoded character count of a doctrine document.
///
/// Invalid UTF-8 sequences become one replacement scalar each; NUL-bearing
/// binary documents are refused.
pub fn measure_repository_document(
    path: &str,
    bytes: &[u8],
) -> Result<RepositoryDocumentMeasurement, ParseError> {
    let path = canonical_path(path)?;
    if bytes.contains(&0) {
        return Err(ParseError::unsupported(
            "policy.repo_truth.docs",
            format!("selected repository document {path:?} contains a NUL byte"),
            "remove the binary path from repository-truth docs or replace it with text",
        ));
    }
    let characters = String::from_utf8_lossy(bytes).chars().count();
    Ok(RepositoryDocumentMeasurement {
        path,
        characters: u64::try_from(characters).expect("decoded character count fits in u64"),
    })
}
