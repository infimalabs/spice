//! The console's speech, spoken by the shell's own synthesizer.
//!
//! WebKit publishes a voice to a page's `speechSynthesis` only when the system
//! marks it a system voice, so the Premium, Enhanced, and Siri voices the
//! operator downloaded never reach a page in a `WKWebView`, and no entitlement
//! lifts that filter. The shell owns an `AVSpeechSynthesizer` instead and
//! offers the page `window.spiceShellSpeech`, which the console's speech
//! controller prefers over `speechSynthesis` whenever it exists:
//!
//! - `getVoices()` lists every installed voice as `identifier`, `name`,
//!   `language`, `quality` (`default`, `enhanced`, or `premium`), and whether
//!   it is the `default` voice of the system language. The list is empty until
//!   the shell publishes it, which then calls `onvoiceschanged`.
//! - `speak(text, identifier, rate, handlers)` replaces the current utterance.
//!   An empty identifier speaks the system language's default voice; `rate` is
//!   Web Speech's multiplier, mapped exactly as WebKit maps it. The handlers'
//!   `onstart`, `onend`, and `onerror({ error })` report that utterance alone.
//! - `cancel()` silences the current utterance; its handlers never fire again.
//!
//! The page posts requests over wry's IPC channel and the shell answers by
//! evaluating script, which the page's content security policy does not
//! govern. The surface carries no token: only the console's own document runs
//! in this webview, and new windows leave for the system browser.

mod avf;

use avf::{
    AVSpeechSynthesisVoice, AVSpeechSynthesizer, AVSpeechSynthesizerDelegate, AVSpeechUtterance,
    AVSpeechUtteranceDefaultSpeechRate, AVSpeechUtteranceMaximumSpeechRate, BOUNDARY_IMMEDIATE,
    QUALITY_ENHANCED, QUALITY_PREMIUM,
};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSObjectProtocol, NSString};
use wry::WebView;

/// The page's side of the surface. Each document asks for the voices once it
/// starts, which also silences whatever an earlier document left speaking.
pub(super) const SCRIPT: &str = r#"(() => {
  let voices = [];
  let current = null;
  let next = 0;
  const speech = {
    onvoiceschanged: null,
    getVoices: () => voices.slice(),
    speak(text, identifier, rate, handlers) {
      current = { id: ++next, handlers };
      window.ipc.postMessage(`speech-speak\n${current.id}\n${rate}\n${identifier}\n${text}`);
    },
    cancel() {
      current = null;
      window.ipc.postMessage("speech-cancel");
    },
  };
  Object.defineProperties(speech, {
    published: {
      value(list) {
        voices = list;
        speech.onvoiceschanged?.();
      },
    },
    utterance: {
      value(id, kind, error) {
        if (current?.id !== id) return;
        const handlers = current.handlers;
        if (kind !== "start") current = null;
        handlers[`on${kind}`]?.(kind === "error" ? { error } : undefined);
      },
    },
  });
  Object.defineProperty(window, "spiceShellSpeech", { value: speech });
  window.ipc.postMessage("speech-ready");
})();"#;

/// What the page asks of the synthesizer, or what the synthesizer reports.
pub(super) enum Event {
    /// A document started: silence what an earlier one queued and publish
    /// the installed voices to this one.
    Ready,
    /// Speak `text` as the page's utterance `id`, replacing the current one.
    Speak {
        id: u64,
        rate: f32,
        voice: String,
        text: String,
    },
    /// Silence the current utterance.
    Cancel,
    /// The synthesizer moved the utterance at this address.
    Progress { utterance: usize, step: Step },
}

impl Event {
    /// The request one IPC message names, if any. A spoken text is the last
    /// field, so it keeps its own line breaks.
    pub(super) fn parse(message: &str) -> Option<Self> {
        match message {
            "speech-ready" => return Some(Self::Ready),
            "speech-cancel" => return Some(Self::Cancel),
            _ => {}
        }
        let mut fields = message.strip_prefix("speech-speak\n")?.splitn(4, '\n');
        let id = fields.next()?.parse().ok()?;
        let rate = fields.next()?.parse().ok()?;
        let voice = fields.next()?.to_owned();
        let text = fields.next()?.to_owned();
        Some(Self::Speak {
            id,
            rate,
            voice,
            text,
        })
    }
}

/// How far the synthesizer has taken one utterance.
#[derive(Clone, Copy)]
pub(super) enum Step {
    Started,
    /// Finished or cancelled; WebKit reports both as the utterance's end.
    Ended,
}

/// The window's synthesizer and the utterances it still owes callbacks for.
pub(super) struct Speech {
    synthesizer: Retained<AVSpeechSynthesizer>,
    /// The synthesizer holds its delegate weakly; this keeps it alive.
    _delegate: Retained<Delegate>,
    /// Every utterance handed to the synthesizer that has not ended, with the
    /// page's id for it. Holding each until its last callback keeps its
    /// address from ever naming a newer utterance.
    spoken: Vec<(Retained<AVSpeechUtterance>, u64)>,
}

impl Speech {
    /// A synthesizer whose callbacks, from any thread, go to `report`.
    pub(super) fn new(report: impl Fn(Event) + Send + Sync + 'static) -> Self {
        let delegate = Delegate::new(Box::new(report));
        // SAFETY: a plain synthesizer, given a delegate this value keeps
        // alive for as long as the synthesizer.
        let synthesizer = unsafe {
            let synthesizer = AVSpeechSynthesizer::new();
            synthesizer.set_delegate(Some(ProtocolObject::from_ref(&*delegate)));
            synthesizer
        };
        Self {
            synthesizer,
            _delegate: delegate,
            spoken: Vec::new(),
        }
    }

    /// Answer one page request or synthesizer report.
    pub(super) fn answer(&mut self, event: Event, webview: &WebView) {
        match event {
            Event::Ready => {
                self.stop();
                publish(webview);
            }
            Event::Cancel => self.stop(),
            Event::Speak {
                id,
                rate,
                voice,
                text,
            } => {
                self.stop();
                if let Err(code) = self.speak(id, rate, &voice, &text) {
                    deliver(webview, id, "error", Some(code));
                }
            }
            Event::Progress { utterance, step } => {
                let Some(index) = self
                    .spoken
                    .iter()
                    .position(|(spoken, _)| Retained::as_ptr(spoken) as usize == utterance)
                else {
                    return;
                };
                let id = self.spoken[index].1;
                let kind = match step {
                    Step::Started => "start",
                    Step::Ended => {
                        self.spoken.swap_remove(index);
                        "end"
                    }
                };
                deliver(webview, id, kind, None);
            }
        }
    }

    fn speak(&mut self, id: u64, rate: f32, voice: &str, text: &str) -> Result<(), &'static str> {
        // SAFETY: class lookups and property writes on a fresh utterance the
        // synthesizer then retains until it ends.
        unsafe {
            let voice = match voice {
                "" => None,
                identifier => Some(
                    AVSpeechSynthesisVoice::with_identifier(&NSString::from_str(identifier))
                        .ok_or("voice-unavailable")?,
                ),
            };
            let utterance = AVSpeechUtterance::with_string(&NSString::from_str(text));
            utterance.set_voice(voice.as_deref());
            utterance.set_rate(platform_rate(rate));
            self.spoken.push((utterance.clone(), id));
            self.synthesizer.speak(&utterance);
        }
        Ok(())
    }

    fn stop(&self) {
        // SAFETY: stopping a live synthesizer; it then cancels each queued
        // utterance through the delegate.
        unsafe {
            self.synthesizer.stop_at(BOUNDARY_IMMEDIATE);
        }
    }
}

impl Drop for Speech {
    fn drop(&mut self) {
        self.stop();
        // SAFETY: detaching the delegate this value is about to release.
        unsafe { self.synthesizer.set_delegate(None) };
    }
}

/// Web Speech's rate multiplier as `AVSpeechUtterance` reads it, mapped as
/// WebKit maps it for `speechSynthesis`, so the shell speaks at the browser's
/// pace: below one scales the default rate, above one climbs toward the
/// maximum.
fn platform_rate(rate: f32) -> f32 {
    // SAFETY: immutable framework constants.
    let (default, maximum) = unsafe {
        (
            AVSpeechUtteranceDefaultSpeechRate,
            AVSpeechUtteranceMaximumSpeechRate,
        )
    };
    if rate < 1.0 {
        rate * default
    } else {
        default + (rate - 1.0) * (maximum - default)
    }
}

/// Hand every installed voice to the page.
fn publish(webview: &WebView) {
    // SAFETY: class lookups and property reads on the system's voices.
    let voices: Vec<serde_json::Value> = unsafe {
        let default = AVSpeechSynthesisVoice::with_language(Some(
            &AVSpeechSynthesisVoice::current_language_code(),
        ))
        .map(|voice| voice.identifier());
        AVSpeechSynthesisVoice::speech_voices()
            .iter()
            .map(|voice| {
                let identifier = voice.identifier();
                let quality = match voice.quality() {
                    QUALITY_PREMIUM => "premium",
                    QUALITY_ENHANCED => "enhanced",
                    _ => "default",
                };
                serde_json::json!({
                    "identifier": identifier.to_string(),
                    "name": voice.name().to_string(),
                    "language": voice.language().to_string(),
                    "quality": quality,
                    "default": default.as_deref() == Some(&*identifier),
                })
            })
            .collect()
    };
    let _ = webview.evaluate_script(&format!(
        "window.spiceShellSpeech.published({})",
        serde_json::Value::Array(voices)
    ));
}

/// Report one utterance's progress to the page.
fn deliver(webview: &WebView, id: u64, kind: &str, error: Option<&str>) {
    let _ = webview.evaluate_script(&format!(
        "window.spiceShellSpeech.utterance({id}, {}, {})",
        serde_json::json!(kind),
        serde_json::json!(error),
    ));
}

struct DelegateIvars {
    report: Box<dyn Fn(Event) + Send + Sync>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SpiceShellSpeechDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl AVSpeechSynthesizerDelegate for Delegate {
        #[unsafe(method(speechSynthesizer:didStartSpeechUtterance:))]
        fn did_start(&self, _synthesizer: &AVSpeechSynthesizer, utterance: &AVSpeechUtterance) {
            self.report(utterance, Step::Started);
        }

        #[unsafe(method(speechSynthesizer:didFinishSpeechUtterance:))]
        fn did_finish(&self, _synthesizer: &AVSpeechSynthesizer, utterance: &AVSpeechUtterance) {
            self.report(utterance, Step::Ended);
        }

        #[unsafe(method(speechSynthesizer:didCancelSpeechUtterance:))]
        fn did_cancel(&self, _synthesizer: &AVSpeechSynthesizer, utterance: &AVSpeechUtterance) {
            self.report(utterance, Step::Ended);
        }
    }
);

impl Delegate {
    fn new(report: Box<dyn Fn(Event) + Send + Sync>) -> Retained<Self> {
        let delegate = Self::alloc().set_ivars(DelegateIvars { report });
        // SAFETY: plain NSObject init on a freshly allocated instance whose
        // ivars are set.
        unsafe { msg_send![super(delegate), init] }
    }

    fn report(&self, utterance: &AVSpeechUtterance, step: Step) {
        (self.ivars().report)(Event::Progress {
            utterance: std::ptr::from_ref(utterance) as usize,
            step,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::Event;

    #[test]
    fn a_spoken_text_keeps_its_line_breaks_and_a_malformed_request_is_ignored() {
        let Some(Event::Speak {
            id,
            rate,
            voice,
            text,
        }) =
            Event::parse("speech-speak\n7\n1.25\ncom.apple.voice.premium.en-US.Zoe\nfirst\nsecond")
        else {
            panic!("a speak request parses");
        };
        assert_eq!(
            (id, rate, voice.as_str(), text.as_str()),
            (
                7,
                1.25,
                "com.apple.voice.premium.en-US.Zoe",
                "first\nsecond"
            )
        );
        assert!(matches!(
            Event::parse("speech-speak\n8\n1\n\n"),
            Some(Event::Speak { voice, text, .. }) if voice.is_empty() && text.is_empty()
        ));
        assert!(Event::parse("speech-speak\nseven\n1\n\ntext").is_none());
        assert!(Event::parse("speech-speak\n7\n1").is_none());
    }
}
