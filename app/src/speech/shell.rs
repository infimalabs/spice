//! The desktop shell's speech surface, `window.spiceShellSpeech`.
//!
//! WebKit hands a page only the system's built-in voices, so the desktop shell
//! speaks through its own synthesizer and installs this surface for the
//! console; a browser never has it. Its utterance handlers receive what Web
//! Speech's do: `onend` nothing, `onerror` an object whose `error` names the
//! failure.

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue};

/// The shell's synthesizer, as the page sees it.
#[derive(Clone)]
pub(super) struct ShellSpeech(Object);

/// One installed voice.
pub(super) struct ShellVoice {
    /// The system's voice identifier, which carries premium evidence.
    pub(super) identifier: String,
    pub(super) name: String,
    pub(super) language: String,
    /// Whether this is the default voice of the system language.
    pub(super) default: bool,
}

impl ShellSpeech {
    /// The surface, when this page runs inside the desktop shell.
    pub(super) fn installed(window: &web_sys::Window) -> Option<Self> {
        Reflect::get(window, &JsValue::from_str("spiceShellSpeech"))
            .ok()
            .and_then(|surface| surface.dyn_into::<Object>().ok())
            .map(Self)
    }

    /// Every installed voice, empty until the shell publishes them.
    pub(super) fn voices(&self) -> Vec<ShellVoice> {
        self.call("getVoices", &Array::new())
            .and_then(|list| list.dyn_into::<Array>().ok())
            .map(|list| list.iter().map(|voice| ShellVoice::read(&voice)).collect())
            .unwrap_or_default()
    }

    /// Called once the shell publishes its voices.
    pub(super) fn observe_voices(&self, observer: Option<&Function>) {
        let observer = observer.map_or(JsValue::NULL, |observer| observer.clone().into());
        let _ = Reflect::set(&self.0, &JsValue::from_str("onvoiceschanged"), &observer);
    }

    /// Replace the current utterance; an empty identifier speaks the system
    /// language's default voice.
    pub(super) fn speak(&self, text: &str, identifier: &str, rate: f32, handlers: &Object) {
        let arguments = Array::of4(
            &JsValue::from_str(text),
            &JsValue::from_str(identifier),
            &JsValue::from_f64(f64::from(rate)),
            handlers,
        );
        let _ = self.call("speak", &arguments);
    }

    /// Silence the current utterance; its handlers never fire again.
    pub(super) fn cancel(&self) {
        let _ = self.call("cancel", &Array::new());
    }

    fn call(&self, method: &str, arguments: &Array) -> Option<JsValue> {
        Reflect::get(&self.0, &JsValue::from_str(method))
            .ok()?
            .dyn_into::<Function>()
            .ok()?
            .apply(&self.0, arguments)
            .ok()
    }
}

impl ShellVoice {
    fn read(voice: &JsValue) -> Self {
        let field = |name: &str| Reflect::get(voice, &JsValue::from_str(name)).ok();
        let text = |name: &str| {
            field(name)
                .and_then(|value| value.as_string())
                .unwrap_or_default()
        };
        Self {
            identifier: text("identifier"),
            name: text("name"),
            language: text("language"),
            default: field("default").and_then(|value| value.as_bool()) == Some(true),
        }
    }
}
