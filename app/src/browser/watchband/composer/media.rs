//! Retained message media and their quote-action drawers.

use std::rc::Rc;

use yew::prelude::*;

pub(super) fn render_agent_message_media(
    unlinked_media: Vec<&wire::Media>,
    all_media: &[wire::Media],
    description: Option<&str>,
    media_quotes: &[wire::MediaQuote],
) -> Html {
    if unlinked_media.is_empty() {
        return Html::default();
    }
    html! {
        <div class="message-attachments">
            {for unlinked_media.into_iter().map(|media| {
                let ordinal = all_media
                    .iter()
                    .position(|candidate| std::ptr::eq(candidate, media));
                render_media_button(
                    media,
                    description,
                    None,
                    ordinal.and_then(|ordinal| media_quote_for(media, media_quotes, ordinal)),
                )
            })}
        </div>
    }
}

/// A crystallized run: the first card's image, then each companion's, every
/// one in its own cell and openable full size on its own.
pub(super) fn render_crystal_media(
    message: &wire::AgentMessage,
    companions: &[Rc<wire::AgentMessage>],
    crystal: &NodeRef,
) -> Html {
    let count = 1 + companions.len();
    html! {
        <div ref={crystal.clone()} class="message-attachments mosaic-crystal" data-crystal-count={count.to_string()}>
            {for std::iter::once(message).chain(companions.iter().map(Rc::as_ref)).map(|message| {
                let description = (!message.text.trim().is_empty())
                    .then(|| crate::watchband::tool_media_description(&message.text));
                html! {
                    <>{for message.media.iter().enumerate().map(|(ordinal, media)| {
                        render_media_button(
                            media,
                            description.as_deref(),
                            Some("mosaic-crystal-cell"),
                            media_quote_for(media, &message.media_quotes, ordinal),
                        )
                    })}</>
                }
            })}
        </div>
    }
}

fn render_media_button(
    media: &wire::Media,
    description: Option<&str>,
    cell: Option<&'static str>,
    media_quote: Option<&wire::MediaQuote>,
) -> Html {
    let description = description.unwrap_or(&media.name).to_owned();
    let identity = media
        .identity()
        .expect("admitted message media has identity")
        .to_string();
    let action = format!("Open {description} full size");
    let preview = html! {
        <button
            type="button"
            class={classes!("message-image", "message-image--openable")}
            title={action.clone()}
            aria-label={action}
            data-image-preview=""
            data-image-name={media.name.clone()}
        >
            <img
                src={media.data_url.clone()}
                alt={description}
                data-media-identity={identity.clone()}
                data-mosaic-image-state="pending"
            />
            <span
                class="message-image-status"
                role="status"
                data-mosaic-image-status="pending"
            >
                {"Loading image"}
            </span>
        </button>
    };
    html! {
        <span key={identity} class={classes!("message-image-shell", cell)}>
            {preview}
            if let Some(media_quote) = media_quote {
                <span class="message-actions message-image-actions app-control-rail">
                    {crate::browser::view::chrome::render_action_drawer(html! {
                        <>
                            <button
                                type="button"
                                class="app-card-action app-card-action--draft-quote message-image-quote message-image-quote--draft"
                                data-image-quote-action="draft"
                                data-image-quote-target={media_quote.target.to_string()}
                                data-image-quote-name={media_quote.name.clone()}
                                title={format!("Quote {} into a new Draft", media_quote.name)}
                                aria-label={format!("Quote {} into a new Draft", media_quote.name)}
                            >{Html::from_html_unchecked(AttrValue::Static(crate::composer_markdown::DRAFT_QUOTE_ICON))}</button>
                            <button
                                type="button"
                                class="app-card-action app-card-action--quote message-image-quote"
                                data-image-quote-action=""
                                data-image-quote-target={media_quote.target.to_string()}
                                data-image-quote-name={media_quote.name.clone()}
                                title={format!("Quote {}", media_quote.name)}
                                aria-label={format!("Quote {}", media_quote.name)}
                            >{Html::from_html_unchecked(AttrValue::Static(crate::composer_markdown::QUOTE_ICON))}</button>
                        </>
                    })}
                </span>
            }
        </span>
    }
}

fn media_quote_for<'a>(
    media: &wire::Media,
    media_quotes: &'a [wire::MediaQuote],
    ordinal: usize,
) -> Option<&'a wire::MediaQuote> {
    let ordinal = u8::try_from(ordinal).ok()?;
    let identity = media.identity().ok()?;
    media_quotes.iter().find(|quote| {
        quote.name == media.name
            && *quote.target.digest() == identity
            && match &quote.target {
                wire::QuoteTarget::TapeMedia {
                    ordinal: target, ..
                }
                | wire::QuoteTarget::SteeringMedia {
                    ordinal: target, ..
                } => *target == ordinal,
                wire::QuoteTarget::Tape { .. }
                | wire::QuoteTarget::Steering { .. }
                | wire::QuoteTarget::Room { .. } => false,
            }
    })
}
