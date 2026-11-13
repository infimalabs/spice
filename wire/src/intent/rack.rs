//! Bounded read-only views of the documentation linked into the resident's rack.

use serde::{Deserialize, Serialize};

use crate::{Digest, Media, SequenceCode};

/// Largest UTF-8 rack document one app response carries.
pub const RACK_TEXT_BYTES_MAX: usize = 512 * 1024;
/// Largest normalized path relative to the rack's `docs` root.
pub const RACK_PATH_BYTES_MAX: usize = 4 * 1024;
/// Largest immediate directory listing one response carries.
pub const RACK_DIRECTORY_ENTRIES_MAX: usize = 1024;

/// Read one directory or member beneath the linked rack's `docs` root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RackRead {
    /// Normalized path relative to `docs`; empty names the root.
    pub path: String,
    /// Optional short code of the journal fact that admitted the linked rack.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub generation: Option<SequenceCode>,
}

/// Stable identity of the rack that answered one read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RackIdentity {
    /// Digest of the exact linked rack archive.
    pub archive: Digest,
    /// Short code of its admitting journal fact, when one exists.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub generation: Option<SequenceCode>,
}

/// What one immediate child of a rack directory is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RackEntryKind {
    /// A directory containing more members.
    Directory,
    /// A UTF-8 Markdown or plain-text document the app may render.
    Document,
    /// A raster image the app may request.
    Image,
    /// A member the app intentionally does not serve.
    Unsupported,
}

/// One immediate child in stable rack path order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RackEntry {
    /// Normalized child path relative to `docs`.
    pub path: String,
    /// Member class admitted by the app surface.
    pub kind: RackEntryKind,
    /// Declared content type for a member; absent for directories or unknown extensions.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub content_type: Option<String>,
    /// Uncompressed member bytes; absent for directories.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub bytes: Option<u64>,
}

/// Directory or member content returned by one rack read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RackContent {
    /// Immediate children of a directory.
    Directory { entries: Vec<RackEntry> },
    /// One bounded UTF-8 Markdown or plain-text member.
    Text { content_type: String, text: String },
    /// One admitted raster image.
    Image { media: Media },
}

/// Successful answer to [`RackRead`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RackAnswer {
    /// Rack generation that supplied the content.
    pub identity: RackIdentity,
    /// Canonical normalized path relative to `docs`.
    pub path: String,
    /// Directory or member content.
    pub content: RackContent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::{CommandAnswer, Intent};
    use crate::{CommandKind, Domain};

    #[test]
    fn rack_command_binds_and_round_trips_its_closed_shapes() {
        let request = RackRead {
            path: "guide.md".to_owned(),
            generation: Some(SequenceCode::new(42)),
        };
        let binding = CommandKind::RackRead.binding();
        let encoded = crate::encode(&request).expect("request encodes");
        assert_eq!(
            binding.body.decode(&encoded).expect("request decodes"),
            Intent::RackRead(request.clone())
        );
        assert!(
            binding
                .body
                .decode(br#"{"path":"guide.md","generation":42,"extra":true}"#)
                .is_err()
        );

        let answer = RackAnswer {
            identity: RackIdentity {
                archive: Digest::compute(Domain::DistArtifact, b"rack"),
                generation: request.generation,
            },
            path: request.path,
            content: RackContent::Text {
                content_type: "text/markdown; charset=utf-8".to_owned(),
                text: "# Guide".to_owned(),
            },
        };
        let encoded = crate::encode(&answer).expect("answer encodes");
        assert_eq!(
            binding.answer.decode(&encoded).expect("answer decodes"),
            CommandAnswer::Rack(Box::new(answer))
        );
        assert!(CommandKind::RackRead.is_read());
        assert_eq!(CommandKind::RackRead.law(), crate::Law::OPERATOR_READ);
    }
}
