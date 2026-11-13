use std::borrow::Cow;

use super::*;
use crate::browser::mermaid::MarkdownControl;

struct MessageControlContext<'a> {
    message: &'a wire::AgentMessage,
    quote_lane: CanonicalUuid,
    quote_dispatcher: &'a UseReducerDispatcher<ComposerDrafts>,
    speak: &'a Callback<(SpeechControl, String)>,
    speech_owner: Option<&'a SpeechOwner>,
    message_key: &'a str,
    order: u64,
    producer_label: &'a str,
    attribution: &'a str,
}

fn collect_quote_controls(
    quotes: &[wire::QuotePresentation],
    quote_sources: &crate::composer_markdown::QuoteSources,
    context: &MessageControlContext<'_>,
    controls: &mut Vec<MarkdownControl>,
) {
    for quote in quotes {
        let wire::QuotePresentation::Resolved {
            target,
            text,
            quotes,
            ..
        } = quote
        else {
            continue;
        };
        let Some(quote_target) = target.parse::<wire::QuoteTarget>().ok() else {
            continue;
        };
        let display_millis = quote_sources.get(target).map_or_else(
            || context.message.display_millis(),
            |source| source.display_millis,
        );
        let source_id = quote_sources
            .get(target)
            .and_then(|source| source.card_id.clone())
            .unwrap_or_else(|| agent_message_source_id(context.message));
        let source = QuoteControlSource {
            target: quote_target,
            source_id,
            target_text: target,
            text: Cow::Borrowed(text),
            preview: watchband::first_nonquoted_prose(text),
            display_millis,
        };
        push_quote_control(source, context, controls);
        collect_quote_controls(quotes, quote_sources, context, controls);
    }
}

fn collect_directive_controls(
    directives: &[wire::MessageDirective],
    text: &str,
    context: &MessageControlContext<'_>,
    controls: &mut Vec<MarkdownControl>,
) {
    for directive in directives {
        let (Ok(start), Ok(end)) = (
            usize::try_from(directive.start),
            usize::try_from(directive.end),
        ) else {
            continue;
        };
        let Some(text) = text.get(start..end) else {
            continue;
        };
        let target_text = directive.quote_target.to_string();
        let (text, preview) =
            watchband::directive_controls::directive_control_prose(directive.review.as_ref(), text);
        let source = QuoteControlSource {
            target: directive.quote_target.clone(),
            source_id: agent_message_source_id(context.message),
            target_text: &target_text,
            text,
            preview,
            display_millis: directive.authored.get().as_millisecond(),
        };
        push_quote_control(source, context, controls);
    }
}

/// The hunk one quote control sits on: its authenticated target and the card
/// the quote links back to, the rendered text its speak action reads, and how
/// that text yields the authored line its composer shard shows.
struct QuoteControlSource<'a> {
    target: wire::QuoteTarget,
    source_id: String,
    target_text: &'a str,
    text: Cow<'a, str>,
    preview: Option<String>,
    display_millis: i64,
}

fn push_quote_control(
    source: QuoteControlSource<'_>,
    context: &MessageControlContext<'_>,
    controls: &mut Vec<MarkdownControl>,
) {
    let QuoteControlSource {
        target,
        source_id,
        target_text,
        text,
        preview,
        display_millis,
    } = source;
    let quote = {
        let lane = context.quote_lane.to_string();
        let dispatcher = context.quote_dispatcher.clone();
        let preview = watchband::quote_label(preview.as_deref()).to_owned();
        let order = context.order;
        Callback::from(move |destination| {
            dispatch_quote(
                &dispatcher,
                ComposerAction::AddQuote {
                    lane: lane.clone(),
                    destination,
                    target: target.clone(),
                    source_id: source_id.clone(),
                    order,
                    display_millis,
                    preview: preview.clone(),
                },
            );
        })
    };
    let correspondent = view::chrome::Correspondent {
        accent: 0,
        speak: context.speak.clone(),
        speech_owner: context.speech_owner.cloned(),
        message_key: context.message_key.to_owned(),
        quote: Some(quote),
    };
    controls.push(MarkdownControl {
        target: AttrValue::from(target_text.to_owned()),
        content: render_message_controls(
            context.message,
            text,
            &correspondent,
            true,
            context.producer_label,
            context.attribution,
        ),
    });
}

pub(super) struct MessageChrome {
    pub(super) signoff: Option<Html>,
    pub(super) controls: Rc<Vec<MarkdownControl>>,
    pub(super) corner: Option<Html>,
}

pub(super) fn render_message_chrome(
    props: &AgentMessageCardProps,
    shown_text: &str,
    signed_body: bool,
    lone_tool_image: bool,
    correspondent: &view::chrome::Correspondent,
    producer_label: &str,
    attribution: &str,
) -> MessageChrome {
    let message = &props.message;
    let signoff =
        signed_body.then(|| render_agent_message_signoff(message, producer_label, attribution));
    let mut controls = Vec::new();
    if signed_body {
        controls.push(MarkdownControl {
            target: AttrValue::from(""),
            content: render_message_controls(
                message,
                Cow::Borrowed(shown_text),
                correspondent,
                true,
                producer_label,
                attribution,
            ),
        });
    }
    let control_context = MessageControlContext {
        message,
        quote_lane: props.quote_lane,
        quote_dispatcher: &props.quote_dispatcher,
        speak: &correspondent.speak,
        speech_owner: correspondent.speech_owner.as_ref(),
        message_key: &correspondent.message_key,
        order: props.order,
        producer_label,
        attribution,
    };
    collect_quote_controls(
        &message.quotes,
        &props.quote_sources,
        &control_context,
        &mut controls,
    );
    collect_directive_controls(
        &message.directive_contexts,
        shown_text,
        &control_context,
        &mut controls,
    );
    // A direction card is its quote, whose own lead carries the age, the
    // operator → receiver signoff, and the controls. Every other unsigned body
    // keeps its metadata in the corner island.
    let corner = (!signed_body
        && !lone_tool_image
        && message.activity != wire::AgentActivityKind::Direction)
        .then(|| {
            render_message_controls(
                message,
                Cow::Borrowed(shown_text),
                correspondent,
                false,
                producer_label,
                attribution,
            )
        });
    MessageChrome {
        signoff,
        controls: Rc::new(controls),
        corner,
    }
}
