//! Preserve block structure while exposing renderer-owned slots to message cards.

use pulldown_cmark::{CodeBlockKind, CowStr, Event, Tag, TagEnd};

pub(super) fn extract<'a>(events: Vec<Event<'a>>) -> (Vec<Event<'a>>, Vec<String>) {
    let mut output = Vec::with_capacity(events.len());
    let mut diagrams = Vec::new();
    let mut source = None::<String>;
    for event in events {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info.split_whitespace().next() == Some("mermaid") =>
            {
                source = Some(String::new());
            }
            Event::Text(text) if source.is_some() => {
                source
                    .as_mut()
                    .expect("active Mermaid fence")
                    .push_str(&text);
            }
            Event::End(TagEnd::CodeBlock) if source.is_some() => {
                let index = diagrams.len();
                diagrams.push(source.take().expect("closed Mermaid fence"));
                output.push(Event::Html(CowStr::from(format!(
                    "<div class=\"mermaid-slot\" data-mermaid-slot=\"{index}\"></div>\n"
                ))));
            }
            event => output.push(event),
        }
    }
    (output, diagrams)
}
