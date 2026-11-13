use super::*;
use crate::browser::{clipboard, toast};
use wasm_bindgen_futures::{JsFuture, spawn_local};

mod export;

#[derive(Clone, Copy)]
enum Action {
    Copy,
    Download,
}

#[derive(Properties, PartialEq)]
pub(super) struct ActionsProps {
    pub source: AttrValue,
    pub name: AttrValue,
    pub link: Option<AttrValue>,
    pub onclose: Callback<()>,
    pub image_status: AttrValue,
    pub onedit: Option<Callback<()>>,
}

/// Why `action` cannot run on this image, or `None` when it can.
fn refusal(action: Action, exportable: bool, copy_refusal: Option<&String>) -> Option<String> {
    if !exportable {
        Some("This image's origin does not allow copy or download.".to_owned())
    } else if matches!(action, Action::Copy) {
        copy_refusal.cloned()
    } else {
        None
    }
}

#[function_component(Actions)]
pub(super) fn actions(props: &ActionsProps) -> Html {
    let busy = use_state_eq(|| false);
    // Results show inside the dialog, beside the image they report on, through
    // the lightbox's own toast instance.
    let (status, shown, notify) = toast::use_toast();
    let copy_refusal = use_memo((), |()| clipboard::image_refusal());
    let exportable = use_memo(props.source.clone(), |source| export::readable(source));
    let perform = use_callback(
        (
            props.source.clone(),
            props.name.clone(),
            busy.clone(),
            notify,
            copy_refusal.clone(),
            exportable.clone(),
        ),
        |action: Action, (source, name, busy, notify, copy_refusal, exportable)| {
            // An unavailable action stays activatable so the operator learns
            // why only when asking, rather than from a standing notice.
            if let Some(refusal) = refusal(action, **exportable, copy_refusal.as_ref().as_ref()) {
                notify.settled(refusal);
                return;
            }
            busy.set(true);
            notify.pending(match action {
                Action::Copy => "Copying image",
                Action::Download => "Preparing download",
            });
            // Clipboard.write must start inside the trusted click, before awaiting conversion.
            let operation = match action {
                Action::Copy => export::copy(source.clone()),
                Action::Download => Ok(export::download(source.clone(), name.clone())),
            };
            let busy = busy.clone();
            let notify = notify.clone();
            spawn_local(async move {
                let result = match operation {
                    Ok(promise) => JsFuture::from(promise).await,
                    Err(error) => Err(error),
                };
                busy.set(false);
                match result {
                    Ok(_) => notify.settled(match action {
                        Action::Copy => "Image copied as PNG",
                        Action::Download => "Download started",
                    }),
                    Err(error) => {
                        let message = clipboard::error_text(&error);
                        notify.settled(format!(
                            "{} failed: {message}",
                            match action {
                                Action::Copy => "Copy",
                                Action::Download => "Download",
                            }
                        ));
                    }
                }
            });
        },
    );
    let copy = use_callback(perform.clone(), |(), perform| perform.emit(Action::Copy));
    let download = use_callback(perform, |(), perform| perform.emit(Action::Download));
    let copy_button = use_button(copy);
    let download_button = use_button(download);
    let close_button = use_button(props.onclose.clone());
    let edit_button = use_button(props.onedit.clone().unwrap_or_default());
    // A showing action result outranks the image's own load state; once it
    // fades, its text stays so the toast leaves without collapsing first.
    let action_shown = *shown && !status.is_empty();
    let (message, visible) = if action_shown || props.image_status.is_empty() {
        ((*status).clone(), action_shown)
    } else {
        (props.image_status.clone(), true)
    };
    let copy_unavailable = !*exportable || copy_refusal.is_some();
    let download_unavailable = !*exportable;
    html! {
        <>
            <div class="image-lightbox-actions app-control-rail app-overlay-control">
                if props.onedit.is_some() {
                    <button ref={edit_button} type="button" data-image-edit="" aria-label="Annotate image" aria-keyshortcuts="A" title="Annotate image (A)">{"Annotate"}</button>
                }
                if let Some(link) = props.link.clone() {
                    <crate::browser::view::chrome::CopyButton text={link} subject="image link">
                        {"Copy link"}
                    </crate::browser::view::chrome::CopyButton>
                }
                <button ref={copy_button} type="button" disabled={*busy} aria-disabled={copy_unavailable.to_string()} aria-label="Copy image" title="Copy image as PNG">{"Copy"}</button>
                <button ref={download_button} type="button" data-image-download="" disabled={*busy} aria-disabled={download_unavailable.to_string()} aria-label="Download image" aria-keyshortcuts="D" title="Download original image (D)">{"Download"}</button>
                <button ref={close_button} type="button" autofocus=true aria-label="Close full-size image">{"Close"}</button>
            </div>
            {toast::render_toast(message, visible, None)}
        </>
    }
}
