//! Draft image input: the chooser strip, paste, drop, and bounded file reading.

use web_sys::{ClipboardEvent, File, FileList, FileReader};

use super::*;

/// Draft images ride in the shard heading as compact thumbnail chips. Removal is
/// an overlaid control: hovering the image describes the attachment itself
/// instead of raising a wide native "Remove <name>" tooltip across the header.
/// The strip is rendered even with no images so the
/// heading keeps its fixed child order and its grid columns, which holds that
/// final control still as the first image arrives. On touch screens the strip
/// leads with an image chooser: Firefox for Android grants keyboard image
/// insertion only to contenteditable, never to a textarea, so paste and drop
/// cannot be the only way in. A locked strip keeps every control in place,
/// disabled, so locking moves nothing and nothing attaches, removes, or opens.
pub(in crate::browser) fn render_image_indicators(
    lane_key: &str,
    attachments: &[crate::composer_drafts::DraftImage],
    quote_identity: Option<&str>,
    drafts: &UseReducerHandle<ComposerDrafts>,
    locked: bool,
) -> Html {
    let attach_key = lane_key.to_owned();
    let attach_drafts = drafts.clone();
    let attach_quote = quote_identity.map(str::to_owned);
    let attach = Callback::from(move |event: Event| {
        let Some(input) = event.target_dyn_into::<web_sys::HtmlInputElement>() else {
            return;
        };
        if let Some(files) = input.files() {
            add_image_files(&attach_drafts, &attach_key, attach_quote.as_deref(), &files);
        }
        // Clear the selection so choosing the same image again still adds it.
        input.set_value("");
    });
    html! {
        <span class="composer-shard-image-indicators">
            <label class="composer-shard-image-attach" title="Attach image">
                <input type="file" accept="image/*" multiple={true} aria-label="Attach image" disabled={locked} onchange={attach} />
            </label>
            {for attachments.iter().map(|image| {
                let attachment = &image.media;
                let id = image.id;
                let remove_key = lane_key.to_owned();
                let remove_drafts = drafts.clone();
                let remove_quote = quote_identity.map(str::to_owned);
                let remove = Callback::from(move |_| {
                    remove_drafts.dispatch(ComposerAction::RemoveAttachment {
                        lane: remove_key.clone(),
                        quote: remove_quote.clone(),
                        id,
                    });
                });
                html! {
                    <span
                        key={id.to_string()}
                        class="composer-shard-image-indicator"
                        title={attachment.name.clone()}
                    >
                        <button type="button" class="composer-shard-image-open" data-image-preview="" data-image-name={attachment.name.clone()} data-image-lane={lane_key.to_owned()} data-image-quote={quote_identity.map(str::to_owned)} data-image-id={id.to_string()} aria-label={format!("Open {} full size", attachment.name)} disabled={locked}>
                            <img src={attachment.data_url.clone()} alt={attachment.name.clone()} />
                        </button>
                        <button
                            type="button"
                            class="composer-shard-image-remove"
                            title="Remove image"
                            aria-label={format!("Remove image {}", attachment.name)}
                            disabled={locked}
                            onclick={remove}
                        >{"×"}</button>
                    </span>
                }
            })}
        </span>
    }
}

pub(in crate::browser) fn paste_images(
    lane_key: String,
    quote_identity: Option<String>,
    drafts: UseReducerHandle<ComposerDrafts>,
) -> Callback<Event> {
    Callback::from(move |event: Event| {
        let clipboard: ClipboardEvent = event.clone().unchecked_into();
        let Some(transfer) = clipboard.clipboard_data() else {
            return;
        };
        let Some(files) = transfer.files() else {
            return;
        };
        if has_image(&files) {
            clipboard.prevent_default();
            add_image_files(&drafts, &lane_key, quote_identity.as_deref(), &files);
        }
    })
}

/// Image files dropped onto a composer other than a lane's, read and bounded
/// exactly as a paste is. Anything else falls through to the textarea's own
/// drop.
pub(in crate::browser) fn drop_images(
    lane_key: String,
    drafts: UseReducerHandle<ComposerDrafts>,
) -> Callback<DragEvent> {
    Callback::from(move |event: DragEvent| {
        let Some(files) = event.data_transfer().and_then(|transfer| transfer.files()) else {
            return;
        };
        if has_image(&files) {
            event.prevent_default();
            add_image_files(&drafts, &lane_key, None, &files);
        }
    })
}

pub(super) fn has_image(files: &FileList) -> bool {
    (0..files.length()).any(|index| {
        files
            .get(index)
            .is_some_and(|file| file.type_().starts_with("image/"))
    })
}

pub(super) fn add_image_files(
    drafts: &UseReducerHandle<ComposerDrafts>,
    lane_key: &str,
    quote_identity: Option<&str>,
    files: &FileList,
) {
    for index in 0..files.length() {
        let Some(file) = files.get(index) else {
            continue;
        };
        if !file.type_().starts_with("image/") {
            continue;
        }
        if file.size() > MAX_ATTACHMENT_BYTES {
            drafts.dispatch(ComposerAction::Report {
                issue: "Image is over 8 MB.".to_owned(),
            });
            continue;
        }
        read_image(
            drafts.clone(),
            lane_key.to_owned(),
            quote_identity.map(str::to_owned),
            file,
        );
    }
}

fn read_image(
    drafts: UseReducerHandle<ComposerDrafts>,
    lane_key: String,
    quote_identity: Option<String>,
    file: File,
) {
    let Ok(reader) = FileReader::new() else {
        drafts.dispatch(ComposerAction::Report {
            issue: "Image reader is unavailable.".to_owned(),
        });
        return;
    };
    let result_reader = reader.clone();
    let name = if file.name().is_empty() {
        "pasted-image.png".to_owned()
    } else {
        file.name()
    };
    let content_type = if file.type_().is_empty() {
        "image/png".to_owned()
    } else {
        file.type_()
    };
    let onload = Closure::<dyn FnMut(web_sys::ProgressEvent)>::new(move |_| {
        let Ok(value) = result_reader.result() else {
            return;
        };
        let Some(data_url) = value.as_string() else {
            return;
        };
        let attachment = Media {
            name: name.clone(),
            content_type: content_type.clone(),
            data_url,
        };
        let occupied = drafts.0.get(&lane_key).map_or(
            0,
            crate::browser::watchband::state::draft_media_data_url_bytes,
        );
        let available = wire::media::MAX_MEDIA_DATA_URL_BYTES.saturating_sub(occupied);
        let drafts = drafts.clone();
        let lane = lane_key.clone();
        let quote = quote_identity.clone();
        spawn_local(async move {
            match crate::browser::image_media::fit(attachment, available).await {
                Ok(attachment) => drafts.dispatch(ComposerAction::AddAttachment {
                    lane,
                    quote,
                    attachment,
                }),
                Err(error) => drafts.dispatch(ComposerAction::Report {
                    issue: format!("Image was refused: {error}"),
                }),
            }
        });
    });
    reader.set_onload(Some(onload.as_ref().unchecked_ref()));
    if reader.read_as_data_url(&file).is_ok() {
        onload.forget();
    }
}
