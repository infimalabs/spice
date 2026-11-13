//! Held command-modifier state for primary composer submission previews.

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use yew::prelude::*;

const HELD_ATTRIBUTE: &str = "data-command-modifier-held";
const SHIFT_HELD_ATTRIBUTE: &str = "data-shift-modifier-held";

fn set_attribute(root: &web_sys::Element, attribute: &str, held: bool) {
    if root.has_attribute(attribute) == held {
        return;
    }
    if held {
        let _ = root.set_attribute(attribute, "true");
    } else {
        let _ = root.remove_attribute(attribute);
    }
}

fn set_held(root: &web_sys::Element, command: bool, shift: bool) {
    set_attribute(root, HELD_ATTRIBUTE, command);
    set_attribute(root, SHIFT_HELD_ATTRIBUTE, shift);
}

#[hook]
pub(in crate::browser) fn use_command_modifier_preview() {
    use_effect_with((), move |()| {
        let registration = web_sys::window().and_then(|window| {
            let document = window.document()?;
            let root = document.document_element()?;
            let pressed_root = root.clone();
            let keydown = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
                move |event: web_sys::KeyboardEvent| {
                    if !matches!(event.key().as_str(), "Meta" | "Control" | "Shift") {
                        return;
                    }
                    set_held(
                        &pressed_root,
                        event.meta_key() || event.ctrl_key(),
                        event.shift_key(),
                    );
                },
            );
            let released_root = root.clone();
            let keyup = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
                move |event: web_sys::KeyboardEvent| {
                    if !matches!(event.key().as_str(), "Meta" | "Control" | "Shift") {
                        return;
                    }
                    set_held(
                        &released_root,
                        event.meta_key() || event.ctrl_key(),
                        event.shift_key(),
                    );
                },
            );
            let blurred_root = root.clone();
            let blur = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
                set_held(&blurred_root, false, false);
            });
            let _ = document
                .add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref());
            let _ =
                document.add_event_listener_with_callback("keyup", keyup.as_ref().unchecked_ref());
            let _ = window.add_event_listener_with_callback("blur", blur.as_ref().unchecked_ref());
            Some((window, document, root, keydown, keyup, blur))
        });
        move || {
            if let Some((window, document, root, keydown, keyup, blur)) = registration {
                let _ = document.remove_event_listener_with_callback(
                    "keydown",
                    keydown.as_ref().unchecked_ref(),
                );
                let _ = document
                    .remove_event_listener_with_callback("keyup", keyup.as_ref().unchecked_ref());
                let _ = window
                    .remove_event_listener_with_callback("blur", blur.as_ref().unchecked_ref());
                set_held(&root, false, false);
            }
        }
    });
}
