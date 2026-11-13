//! Metadata-only image identities backed by the exact admitted capture record.

use std::path::Path;

use wire::CanonicalUuid;

#[derive(Debug, Eq, PartialEq)]
pub(super) struct Identity {
    content: wire::Digest,
    bytes: usize,
    name: String,
    content_type: String,
}

impl Identity {
    fn of(media: &wire::Media) -> Option<Self> {
        let bytes = media.decoded_bytes().ok()?;
        Some(Self {
            content: wire::Digest::compute(wire::Domain::MediaContent, &bytes),
            bytes: bytes.len(),
            name: media.name.clone(),
            content_type: media.content_type.clone(),
        })
    }

    fn matches(&self, media: &wire::Media) -> Option<bool> {
        let bytes = media.decoded_bytes().ok()?;
        Some(
            self.name == media.name
                && self.content_type == media.content_type
                && self.bytes == bytes.len()
                && self.content == wire::Digest::compute(wire::Domain::MediaContent, &bytes),
        )
    }
}

#[derive(Debug)]
pub(super) struct Image {
    pub(super) identity: Identity,
    locus: listen::Locus,
    ordinal: u32,
}

impl Image {
    pub(super) fn new(media: &wire::Media, locus: listen::Locus, ordinal: u32) -> Option<Self> {
        Some(Self {
            identity: Identity::of(media)?,
            locus,
            ordinal,
        })
    }

    pub(super) fn reacquire(&self, root: &Path, source: CanonicalUuid) -> Option<wire::Media> {
        let tape = tape::Tape::open_existing(root).ok()?;
        let expected: tape::SourceId = source.to_string().parse().ok()?;
        if tape.source_id().ok()? != expected {
            return None;
        }
        // This range was admitted with the original read result. Never scan
        // another record, source, provider directory, or fallback store.
        let bytes = tape.read(self.locus).ok()?;
        let decoded = listen::decode_record(None, &bytes, self.locus)
            .ok()?
            .decoded;
        let listen::Decoded::Events { mut events, .. } = decoded else {
            return None;
        };
        let ordinal = usize::try_from(self.ordinal).ok()?;
        if ordinal >= events.len() {
            return None;
        }
        let listen::Event::Tool(tool) = events.swap_remove(ordinal) else {
            return None;
        };
        let listen::ToolMoment::Result(mut result) = tool.moment else {
            return None;
        };
        if result.result != listen::ToolResult::Succeeded
            || result.media.len() != 1
            || !result.external_media.is_empty()
        {
            return None;
        }
        let media = result.media.pop()?;
        if !self.identity.matches(&media)? || tape.source_id().ok()? != expected {
            return None;
        }
        Some(media)
    }
}
