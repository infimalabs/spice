//! Browser Media Session ownership for durable narration.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;

const IDLE_TITLE: &str = "Spice narration";
const ARTIST: &str = "Spice";
const ALBUM: &str = "Narration";

#[derive(Default)]
struct Posture {
    active: bool,
    speaker: Option<String>,
}

thread_local! {
    static POSTURE: RefCell<Option<Posture>> = const { RefCell::new(None) };
}

/// Synchronize changed platform-session aspects with Narrate and the queue head.
pub(super) fn sync(active: bool, speaker: Option<&str>) {
    let speaker = speaker.filter(|speaker| !speaker.trim().is_empty());
    POSTURE.with(|posture| {
        let mut posture = posture.borrow_mut();
        let playback_changed = posture
            .as_ref()
            .is_none_or(|current| current.active != active);
        let metadata_changed = posture.as_ref().is_none_or(|current| {
            current.active != active || current.speaker.as_deref() != speaker
        });
        if !playback_changed && !metadata_changed {
            return;
        }
        let Some(session) = session() else {
            return;
        };
        sync_on(
            &session,
            active,
            speaker,
            playback_changed,
            metadata_changed,
        );
        *posture = Some(Posture {
            active,
            speaker: speaker.map(str::to_owned),
        });
    });
}

/// Media-key handlers retained for the life of the app page.
pub(super) struct Controls {
    session: JsValue,
    _pause: Closure<dyn FnMut()>,
    _stop: Closure<dyn FnMut()>,
}

impl Controls {
    /// Bind the platform pause and stop actions to the one global speech stop.
    pub(super) fn install(stop: impl Fn() + 'static) -> Option<Self> {
        let session = session()?;
        let stop: Rc<dyn Fn()> = Rc::new(stop);
        let pause_action = Rc::clone(&stop);
        let pause = Closure::<dyn FnMut()>::new(move || pause_action());
        let stop_action = Rc::clone(&stop);
        let stopped = Closure::<dyn FnMut()>::new(move || stop_action());
        set_action_handler(&session, "pause", pause.as_ref());
        set_action_handler(&session, "stop", stopped.as_ref());
        Some(Self {
            session,
            _pause: pause,
            _stop: stopped,
        })
    }
}

impl Drop for Controls {
    fn drop(&mut self) {
        set_action_handler(&self.session, "pause", &JsValue::NULL);
        set_action_handler(&self.session, "stop", &JsValue::NULL);
        sync(false, None);
    }
}

fn session() -> Option<JsValue> {
    let window = web_sys::window()?;
    let navigator = Reflect::get(window.as_ref(), &JsValue::from_str("navigator")).ok()?;
    let session = Reflect::get(&navigator, &JsValue::from_str("mediaSession")).ok()?;
    (!session.is_null() && !session.is_undefined()).then_some(session)
}

fn sync_on(
    session: &JsValue,
    active: bool,
    speaker: Option<&str>,
    playback_changed: bool,
    metadata_changed: bool,
) {
    if playback_changed {
        let state = if active { "playing" } else { "none" };
        let _ = Reflect::set(
            session,
            &JsValue::from_str("playbackState"),
            &JsValue::from_str(state),
        );
    }
    if metadata_changed {
        let metadata = if active {
            let title = speaker.map_or_else(
                || IDLE_TITLE.to_owned(),
                |speaker| format!("{speaker} - {ARTIST}"),
            );
            metadata(&title).unwrap_or(JsValue::NULL)
        } else {
            JsValue::NULL
        };
        let _ = Reflect::set(session, &JsValue::from_str("metadata"), &metadata);
    }
}

fn metadata(title: &str) -> Option<JsValue> {
    let window = web_sys::window()?;
    let constructor = Reflect::get(window.as_ref(), &JsValue::from_str("MediaMetadata"))
        .ok()?
        .dyn_into::<Function>()
        .ok()?;
    let init = Object::new();
    for (name, value) in [("title", title), ("artist", ARTIST), ("album", ALBUM)] {
        Reflect::set(
            init.as_ref(),
            &JsValue::from_str(name),
            &JsValue::from_str(value),
        )
        .ok()?;
    }
    Reflect::construct(&constructor, &Array::of1(init.as_ref())).ok()
}

fn set_action_handler(session: &JsValue, action: &str, handler: &JsValue) {
    let Some(method) = Reflect::get(session, &JsValue::from_str("setActionHandler"))
        .ok()
        .and_then(|method| method.dyn_into::<Function>().ok())
    else {
        return;
    };
    let _ = method.call2(session, &JsValue::from_str(action), handler);
}
