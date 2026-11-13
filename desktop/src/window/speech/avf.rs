//! The AVFAudio speech declarations the shell calls.
//!
//! The workspace's admitted dependency sources carry no AVFAudio binding
//! crate, and the shell needs only these few selectors, so it declares them
//! itself exactly as the framework's headers do.

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{extern_class, extern_methods, extern_protocol};
use objc2_foundation::{NSArray, NSInteger, NSString};

#[link(name = "AVFAudio", kind = "framework")]
unsafe extern "C" {
    pub(super) static AVSpeechUtteranceDefaultSpeechRate: f32;
    pub(super) static AVSpeechUtteranceMaximumSpeechRate: f32;
}

/// `AVSpeechSynthesisVoiceQualityEnhanced`.
pub(super) const QUALITY_ENHANCED: NSInteger = 2;
/// `AVSpeechSynthesisVoiceQualityPremium`.
pub(super) const QUALITY_PREMIUM: NSInteger = 3;
/// `AVSpeechBoundaryImmediate`.
pub(super) const BOUNDARY_IMMEDIATE: NSInteger = 0;

extern_class!(
    #[unsafe(super(NSObject))]
    pub(super) struct AVSpeechSynthesisVoice;
);

impl AVSpeechSynthesisVoice {
    extern_methods!(
        #[unsafe(method(speechVoices))]
        pub(super) unsafe fn speech_voices() -> Retained<NSArray<AVSpeechSynthesisVoice>>;

        #[unsafe(method(currentLanguageCode))]
        pub(super) unsafe fn current_language_code() -> Retained<NSString>;

        #[unsafe(method(voiceWithLanguage:))]
        pub(super) unsafe fn with_language(
            language_code: Option<&NSString>,
        ) -> Option<Retained<AVSpeechSynthesisVoice>>;

        #[unsafe(method(voiceWithIdentifier:))]
        pub(super) unsafe fn with_identifier(
            identifier: &NSString,
        ) -> Option<Retained<AVSpeechSynthesisVoice>>;

        #[unsafe(method(identifier))]
        pub(super) unsafe fn identifier(&self) -> Retained<NSString>;

        #[unsafe(method(name))]
        pub(super) unsafe fn name(&self) -> Retained<NSString>;

        #[unsafe(method(language))]
        pub(super) unsafe fn language(&self) -> Retained<NSString>;

        #[unsafe(method(quality))]
        pub(super) unsafe fn quality(&self) -> NSInteger;
    );
}

extern_class!(
    #[unsafe(super(NSObject))]
    pub(super) struct AVSpeechUtterance;
);

impl AVSpeechUtterance {
    extern_methods!(
        #[unsafe(method(speechUtteranceWithString:))]
        pub(super) unsafe fn with_string(string: &NSString) -> Retained<Self>;

        #[unsafe(method(setVoice:))]
        pub(super) unsafe fn set_voice(&self, voice: Option<&AVSpeechSynthesisVoice>);

        #[unsafe(method(setRate:))]
        pub(super) unsafe fn set_rate(&self, rate: f32);
    );
}

extern_class!(
    #[unsafe(super(NSObject))]
    pub(super) struct AVSpeechSynthesizer;
);

impl AVSpeechSynthesizer {
    extern_methods!(
        #[unsafe(method(new))]
        #[unsafe(method_family = new)]
        pub(super) unsafe fn new() -> Retained<Self>;

        /// The synthesizer holds its delegate weakly.
        #[unsafe(method(setDelegate:))]
        pub(super) unsafe fn set_delegate(
            &self,
            delegate: Option<&ProtocolObject<dyn AVSpeechSynthesizerDelegate>>,
        );

        #[unsafe(method(speakUtterance:))]
        pub(super) unsafe fn speak(&self, utterance: &AVSpeechUtterance);

        #[unsafe(method(stopSpeakingAtBoundary:))]
        pub(super) unsafe fn stop_at(&self, boundary: NSInteger) -> bool;
    );
}

extern_protocol!(
    /// The synthesizer's progress callbacks. Every method is optional.
    ///
    /// # Safety
    ///
    /// An implementing class declares the methods it answers with the
    /// protocol's exact selectors and signatures.
    #[expect(
        clippy::missing_safety_doc,
        reason = "objc2's extern_protocol! re-emits these docs from expansion-spanned attributes, which clippy's doc lints skip, so the # Safety section above is invisible to it"
    )]
    pub(super) unsafe trait AVSpeechSynthesizerDelegate: NSObjectProtocol {}
);
