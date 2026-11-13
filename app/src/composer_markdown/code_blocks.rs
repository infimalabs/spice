//! Box every rendered code block with its corner copy control.
//!
//! The box, not the scrolling `pre`, positions the control, so it stays put
//! while wide code scrolls. `browser::code_copy` copies the `pre` directly
//! inside the control's box. Mermaid fences are already diagram slots here.

use pulldown_cmark::{Event, Tag, TagEnd};

const OPEN: &str = "<div class=\"message-code\"><button type=\"button\" \
     class=\"app-card-action app-card-action--copy message-code-copy\" data-code-copy=\"\" \
     title=\"Copy code\" aria-label=\"Copy code\"><svg class=\"app-card-action__icon\" \
     width=\"15\" height=\"15\" viewBox=\"0 0 24 24\" aria-hidden=\"true\" focusable=\"false\">\
     <path d=\"M9 9h10v10H9V9Zm1.75 1.75v6.5h6.5v-6.5h-6.5ZM5 5h10.5v1.75H6.75v8.75H5V5Z\" \
     fill=\"currentColor\" fill-rule=\"evenodd\" /></svg></button>";
const CLOSE: &str = "</div>\n";

pub(super) fn copyable<'a>(events: impl IntoIterator<Item = Event<'a>>) -> Vec<Event<'a>> {
    let events = events.into_iter();
    let mut output = Vec::with_capacity(events.size_hint().0);
    for event in events {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                output.push(Event::Html(OPEN.into()));
                output.push(event);
            }
            Event::End(TagEnd::CodeBlock) => {
                output.push(event);
                output.push(Event::Html(CLOSE.into()));
            }
            event => output.push(event),
        }
    }
    output
}
