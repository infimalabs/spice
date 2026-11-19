//! Browser Media Session ownership for durable narration.
//!
//! A platform surfaces a page's Media Session (lock screen, notification tray,
//! media hub) only while that page plays an audio or video element; system
//! speech is neither. Narrate therefore keeps one looping audio element playing
//! an inaudible sine for as long as any team selects it, under the exclusive
//! `playback` audio session where the browser offers one, and every narrated
//! entry feeds that one session's metadata.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Function, Object, Reflect, Uint8Array};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use web_sys::{Blob, BlobPropertyBag, HtmlAudioElement, Url};

const IDLE_TITLE: &str = "Spice narration";
const ARTIST: &str = "Spice";
const ALBUM: &str = "Narration";

/// Carrier length: long enough that every platform treats the loop as media
/// worth surfacing rather than a transient sound effect.
const CARRIER_SECONDS: u32 = 6;
const CARRIER_SAMPLE_RATE: u32 = 8_000;
/// A whole number of periods fits the loop (6 s at 40 Hz is 240), so it
/// repeats without a click.
const CARRIER_HZ: f64 = 40.0;
/// Peak of the 16-bit sine, about -78 dBFS: inaudible, yet not the digital
/// silence a platform may treat as no media at all.
const CARRIER_PEAK: f64 = 4.0;
const WAV_HEADER_BYTES: u32 = 44;
const WAV_FORMAT_CHUNK_BYTES: u32 = 16;
const WAV_PCM: u16 = 1;
const WAV_MONO: u16 = 1;
const WAV_SAMPLE_BYTES: u16 = 2;
const WAV_SAMPLE_BITS: u16 = 16;
/// Bytes of the RIFF chunk header ("RIFF" and its size) outside the chunk size.
const RIFF_HEADER_BYTES: u32 = 8;

#[derive(Default, PartialEq)]
struct Posture {
    active: bool,
    speaker: Option<String>,
    title: Option<String>,
}

thread_local! {
    static POSTURE: RefCell<Option<Posture>> = const { RefCell::new(None) };
    static CARRIER: RefCell<Option<HtmlAudioElement>> = const { RefCell::new(None) };
}

/// Synchronize the page's one platform session with Narrate and the queue
/// head: `speaker` becomes the artist and `title` the current entry.
///
/// Activation must run inside the user gesture that selected Narrate, because
/// browsers only let a page start media from one.
pub(super) fn sync(active: bool, speaker: Option<&str>, title: Option<&str>) {
    let next = Posture {
        active,
        speaker: speaker
            .filter(|speaker| !speaker.trim().is_empty())
            .map(str::to_owned),
        title: title
            .filter(|title| !title.trim().is_empty())
            .map(str::to_owned),
    };
    POSTURE.with(|posture| {
        let mut posture = posture.borrow_mut();
        if posture.as_ref() == Some(&next) {
            return;
        }
        let playback_changed = posture
            .as_ref()
            .is_none_or(|current| current.active != active);
        if playback_changed {
            carry(active);
        }
        if let Some(session) = session() {
            sync_on(&session, &next, playback_changed);
        }
        *posture = Some(next);
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
        sync(false, None, None);
    }
}

fn session() -> Option<JsValue> {
    let window = web_sys::window()?;
    let navigator = Reflect::get(window.as_ref(), &JsValue::from_str("navigator")).ok()?;
    let session = Reflect::get(&navigator, &JsValue::from_str("mediaSession")).ok()?;
    (!session.is_null() && !session.is_undefined()).then_some(session)
}

/// Play or pause the one inaudible element that makes the platform surface
/// the page's session.
fn carry(active: bool) {
    set_audio_session(if active { "playback" } else { "auto" });
    CARRIER.with(|carrier| {
        let mut carrier = carrier.borrow_mut();
        if active && carrier.is_none() {
            *carrier = quiet_carrier();
        }
        let Some(element) = carrier.as_ref() else {
            return;
        };
        if active {
            // A refused start (no gesture, as after a back-forward restore)
            // leaves the metadata in place for the next gesture to surface.
            if let Ok(started) = element.play() {
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = wasm_bindgen_futures::JsFuture::from(started).await;
                });
            }
        } else {
            let _ = element.pause();
        }
    });
}

/// Select the Audio Session API type where the browser implements it, so
/// narration owns the device's now-playing session deterministically.
fn set_audio_session(kind: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(navigator) = Reflect::get(window.as_ref(), &JsValue::from_str("navigator")) else {
        return;
    };
    let Ok(audio_session) = Reflect::get(&navigator, &JsValue::from_str("audioSession")) else {
        return;
    };
    if audio_session.is_object() {
        let _ = Reflect::set(
            &audio_session,
            &JsValue::from_str("type"),
            &JsValue::from_str(kind),
        );
    }
}

fn quiet_carrier() -> Option<HtmlAudioElement> {
    let bytes = Uint8Array::from(carrier_wav().as_slice());
    let options = BlobPropertyBag::new();
    options.set_type("audio/wav");
    let blob = Blob::new_with_u8_array_sequence_and_options(&Array::of1(&bytes), &options).ok()?;
    let source = Url::create_object_url_with_blob(&blob).ok()?;
    let element = HtmlAudioElement::new_with_src(&source).ok()?;
    element.set_loop(true);
    Some(element)
}

/// Mono 16-bit PCM of an inaudible low sine.
fn carrier_wav() -> Vec<u8> {
    let samples = CARRIER_SECONDS * CARRIER_SAMPLE_RATE;
    let data_bytes = samples * u32::from(WAV_SAMPLE_BYTES);
    let byte_rate = CARRIER_SAMPLE_RATE * u32::from(WAV_SAMPLE_BYTES);
    let mut wav = Vec::with_capacity((WAV_HEADER_BYTES + data_bytes) as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(WAV_HEADER_BYTES - RIFF_HEADER_BYTES + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&WAV_FORMAT_CHUNK_BYTES.to_le_bytes());
    wav.extend_from_slice(&WAV_PCM.to_le_bytes());
    wav.extend_from_slice(&WAV_MONO.to_le_bytes());
    wav.extend_from_slice(&CARRIER_SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&WAV_SAMPLE_BYTES.to_le_bytes());
    wav.extend_from_slice(&WAV_SAMPLE_BITS.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    let step = std::f64::consts::TAU * CARRIER_HZ / f64::from(CARRIER_SAMPLE_RATE);
    for index in 0..samples {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the rounded sample lies within plus or minus CARRIER_PEAK"
        )]
        let sample = (CARRIER_PEAK * (step * f64::from(index)).sin()).round() as i16;
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

fn sync_on(session: &JsValue, posture: &Posture, playback_changed: bool) {
    if playback_changed {
        let state = if posture.active { "playing" } else { "none" };
        let _ = Reflect::set(
            session,
            &JsValue::from_str("playbackState"),
            &JsValue::from_str(state),
        );
    }
    let metadata = if posture.active {
        metadata(
            posture.title.as_deref().unwrap_or(IDLE_TITLE),
            posture.speaker.as_deref().unwrap_or(ARTIST),
        )
        .unwrap_or(JsValue::NULL)
    } else {
        JsValue::NULL
    };
    let _ = Reflect::set(session, &JsValue::from_str("metadata"), &metadata);
}

fn metadata(title: &str, artist: &str) -> Option<JsValue> {
    let window = web_sys::window()?;
    let constructor = Reflect::get(window.as_ref(), &JsValue::from_str("MediaMetadata"))
        .ok()?
        .dyn_into::<Function>()
        .ok()?;
    let init = Object::new();
    for (name, value) in [("title", title), ("artist", artist), ("album", ALBUM)] {
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
