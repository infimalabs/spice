//! Documentation uses the same sanitized Markdown, diagrams and code controls,
//! with its caller supplying only rack-local links and admitted image payloads.
use super::{BTreeMap, Finish, MarkdownEvent, RenderedMarkdown, Tag, TagEnd};

pub(crate) fn render(markdown: &str, resolve: &dyn Fn(&str, bool) -> String) -> RenderedMarkdown {
    super::render(
        markdown,
        super::Annotations::default(),
        &[],
        &super::QuoteAgents::new(),
        &BTreeMap::new(),
        &[],
        Finish::Document { resolve },
    )
}

pub(super) fn sanitize<'a>(
    event: MarkdownEvent<'a>,
    resolve: &dyn Fn(&str, bool) -> String,
    omitted_image: &mut bool,
) -> MarkdownEvent<'a> {
    let event = match event {
        MarkdownEvent::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => MarkdownEvent::Start(Tag::Link {
            link_type,
            dest_url: resolve(&dest_url, false).into(),
            title,
            id,
        }),
        MarkdownEvent::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            let destination = resolve(&dest_url, true);
            *omitted_image = destination.is_empty();
            if *omitted_image {
                MarkdownEvent::Start(Tag::Emphasis)
            } else {
                MarkdownEvent::Start(Tag::Image {
                    link_type,
                    dest_url: destination.into(),
                    title,
                    id,
                })
            }
        }
        MarkdownEvent::End(TagEnd::Image) if *omitted_image => {
            *omitted_image = false;
            MarkdownEvent::End(TagEnd::Emphasis)
        }
        event => event,
    };
    super::sanitize_markdown_event(event)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn image_sources(markdown: &str) -> Vec<String> {
    let mut sources = Vec::new();
    for event in super::Parser::new_ext(markdown, super::OPTIONS) {
        if let MarkdownEvent::Start(Tag::Image { dest_url, .. }) = event {
            let source = dest_url.into_string();
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
    }
    sources
}
