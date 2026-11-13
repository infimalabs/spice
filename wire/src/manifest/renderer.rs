//! Generation-local renderer files carried by ordinary app receipts.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Digest;

/// Maximum renderer files in one generation.
pub const MAX_FILES: usize = 512;
/// Maximum complete renderer payload admitted into one generation.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Attribute on the trusted app shell declaring its renderer payloads.
pub const ATTRIBUTE: &str = "data-app-renderer-assets";
const MAX_DECLARATION_BYTES: usize = 128 * 1024;

/// Closed renderer response kinds; file declarations cannot supply headers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Document,
    Module,
    Style,
    Text,
    Woff,
    Woff2,
}

impl Kind {
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Document => "html",
            Self::Module => "mjs",
            Self::Style => "css",
            Self::Text => "txt",
            Self::Woff => "woff",
            Self::Woff2 => "woff2",
        }
    }
}

/// A stable, bounded ordinary filename within one immutable generation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    kind: Kind,
    index: u16,
}

impl File {
    #[must_use]
    pub const fn new(kind: Kind, index: u16) -> Option<Self> {
        if (index as usize) < MAX_FILES {
            Some(Self { kind, index })
        } else {
            None
        }
    }

    #[must_use]
    pub const fn kind(self) -> Kind {
        self.kind
    }

    /// Parse only the canonical spelling emitted by this file identity.
    #[must_use]
    pub fn parse_filename(filename: &str) -> Option<Self> {
        let (number, extension) = filename.strip_prefix("renderer-")?.split_once('.')?;
        if number.len() != 3 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let kind = match extension {
            "html" => Kind::Document,
            "mjs" => Kind::Module,
            "css" => Kind::Style,
            "txt" => Kind::Text,
            "woff" => Kind::Woff,
            "woff2" => Kind::Woff2,
            _ => return None,
        };
        Self::new(kind, number.parse().ok()?)
    }
}

impl fmt::Display for File {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "renderer-{:03}.{}",
            self.index,
            self.kind.extension()
        )
    }
}

impl<'de> Deserialize<'de> for File {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            kind: Kind,
            index: u16,
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::new(raw.kind, raw.index)
            .ok_or_else(|| serde::de::Error::custom("renderer file identity exceeds its bound"))
    }
}

/// Exact bytes declared by the already-owned HTML shell.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub file: File,
    /// Content identity under `Domain::InstallArtifact`, as in the receipt.
    pub digest: Digest,
}

/// Validate canonical ordering before any declared file is opened.
pub fn validate(members: &[Member]) -> Result<(), &'static str> {
    if members.len() > MAX_FILES {
        return Err("renderer inventory exceeds its file bound");
    }
    if members.windows(2).any(|pair| pair[0].file >= pair[1].file) {
        return Err("renderer inventory repeats files or is not canonically ordered");
    }
    Ok(())
}

/// Decode the bounded declaration in an already-owned HTML shell.
///
/// A shell without renderer files needs no declaration. This remains the
/// ordinary shell contract; no renderer implementation is selected on error.
pub fn from_html(html: &[u8]) -> Result<Vec<Member>, &'static str> {
    const OPEN: &str = "data-app-renderer-assets=\"";
    let html = std::str::from_utf8(html).map_err(|_| "app shell is not UTF-8")?;
    let declarations = html.matches(ATTRIBUTE).count();
    if declarations > 1 || (declarations == 1 && !html.contains(OPEN)) {
        return Err("renderer declaration has an invalid or repeated attribute");
    }
    let Some((_, after)) = html.split_once(OPEN) else {
        return Ok(Vec::new());
    };
    let (encoded, rest) = after
        .split_once('"')
        .ok_or("renderer declaration is not closed")?;
    if rest.contains(OPEN) || encoded.len() > MAX_DECLARATION_BYTES {
        return Err("renderer declaration is repeated or exceeds its bound");
    }
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
        .map_err(|_| "renderer declaration is not canonical base64")?;
    let members = serde_json::from_slice::<Vec<Member>>(&bytes)
        .map_err(|_| "renderer declaration is not a typed member list")?;
    validate(&members)?;
    Ok(members)
}

/// Encode one canonical renderer declaration for the trusted HTML attribute.
pub fn encode(members: &[Member]) -> Result<String, &'static str> {
    validate(members)?;
    let bytes = serde_json::to_vec(members).map_err(|_| "renderer declaration did not encode")?;
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        bytes,
    ))
}

/// Every exact file the shell declares, including its required base files.
pub fn filenames(members: &[Member]) -> impl Iterator<Item = std::borrow::Cow<'static, str>> + '_ {
    super::APP_FILES
        .into_iter()
        .map(std::borrow::Cow::Borrowed)
        .chain(
            members
                .iter()
                .map(|member| std::borrow::Cow::Owned(member.file.to_string())),
        )
}
