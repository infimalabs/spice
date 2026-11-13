//! Source-scoped correlation between typed image reads and later Markdown.

use std::collections::BTreeMap;
use std::path::Path;

use pulldown_cmark::{Event as MarkdownEvent, LinkType, Options, Parser, Tag};
use serde_json::Value;
use wire::CanonicalUuid;

use super::AgentEvent;

mod captured;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Scope {
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    source: CanonicalUuid,
}

#[derive(Debug)]
enum Association {
    Known(captured::Image),
    Ambiguous,
}

/// Metadata-only image-read state. Bytes are reacquired from the exact
/// admitted capture record rather than retained here.
#[derive(Debug, Default)]
pub(super) struct MediaHydration {
    active: BTreeMap<CanonicalUuid, (CanonicalUuid, CanonicalUuid)>,
    calls: BTreeMap<(Scope, String), String>,
    paths: BTreeMap<(Scope, String), Association>,
}

impl MediaHydration {
    pub(super) fn observe(
        &mut self,
        event: &AgentEvent<'_>,
        media_source: Option<&Path>,
    ) -> Vec<wire::MarkdownImageBinding> {
        let Some(source) = event.source else {
            return Vec::new();
        };
        let scope = Scope {
            lane: event.lane,
            occupant: event.author,
            source,
        };
        self.enter(scope);
        match event.decoded {
            listen::Event::Tool(tool) => {
                self.observe_tool(scope, tool, event.ordinal);
                Vec::new()
            }
            listen::Event::Prose(prose) | listen::Event::StreamedProse(prose)
                if prose.role == listen::ProseRole::Assistant =>
            {
                media_source.map_or_else(Vec::new, |root| self.bindings(scope, &prose.text, root))
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn clear_lane(&mut self, lane: CanonicalUuid) {
        self.active.remove(&lane);
        self.calls.retain(|(scope, _), _| scope.lane != lane);
        self.paths.retain(|(scope, _), _| scope.lane != lane);
    }

    fn enter(&mut self, scope: Scope) {
        let current = (scope.occupant, scope.source);
        if self
            .active
            .get(&scope.lane)
            .is_some_and(|known| *known != current)
        {
            self.clear_lane(scope.lane);
        }
        self.active.insert(scope.lane, current);
    }

    fn observe_tool(&mut self, scope: Scope, tool: &listen::Tool, ordinal: u32) {
        let key = (scope, tool.call_id.clone());
        match &tool.moment {
            listen::ToolMoment::Call {
                name, arguments, ..
            } => {
                if let Some(path) = read_path(name, arguments) {
                    self.calls.insert(key, path);
                } else {
                    self.calls.remove(&key);
                }
            }
            listen::ToolMoment::Update { .. } => {}
            listen::ToolMoment::Result(outcome) => {
                let Some(path) = self.calls.remove(&key) else {
                    return;
                };
                if outcome.result != listen::ToolResult::Succeeded
                    || outcome.media.len() != 1
                    || !outcome.external_media.is_empty()
                {
                    return;
                }
                let identity = captured::Image::new(&outcome.media[0], tool.locus, ordinal);
                let Some(identity) = identity else {
                    return;
                };
                match self.paths.entry((scope, path)) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(Association::Known(identity));
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        if !matches!(entry.get(), Association::Known(known) if known.identity == identity.identity)
                        {
                            entry.insert(Association::Ambiguous);
                        }
                    }
                }
            }
        }
    }

    fn bindings(
        &self,
        scope: Scope,
        markdown: &str,
        media_source: &Path,
    ) -> Vec<wire::MarkdownImageBinding> {
        let mut bindings = Vec::new();
        for destination in image_destinations(markdown) {
            if bindings
                .iter()
                .any(|binding: &wire::MarkdownImageBinding| binding.destination == destination)
            {
                continue;
            }
            let Some(Association::Known(identity)) = self.paths.get(&(scope, destination.clone()))
            else {
                continue;
            };
            let Some(media) = identity.reacquire(media_source, scope.source) else {
                continue;
            };
            bindings.push(wire::MarkdownImageBinding { destination, media });
            if bindings.len() == wire::media::MAX_MEDIA_ITEMS {
                break;
            }
        }
        if wire::media::validate_markdown_image_bindings(&bindings).is_ok() {
            bindings
        } else {
            Vec::new()
        }
    }
}

fn read_path(name: &str, arguments: &str) -> Option<String> {
    if name != "read" {
        return None;
    }
    let Value::Object(fields) = serde_json::from_str::<Value>(arguments).ok()? else {
        return None;
    };
    let path = fields.get("path")?.as_str()?;
    if path.is_empty()
        || path.len() > wire::media::MAX_MEDIA_DESTINATION_BYTES
        || path.chars().any(char::is_control)
        || !Path::new(path).is_absolute()
    {
        return None;
    }
    Some(path.to_owned())
}

fn image_destinations(markdown: &str) -> Vec<String> {
    Parser::new_ext(markdown, Options::all())
        .filter_map(|event| match event {
            MarkdownEvent::Start(Tag::Image {
                link_type: LinkType::Inline,
                dest_url,
                ..
            }) => Some(dest_url.into_string()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests;
