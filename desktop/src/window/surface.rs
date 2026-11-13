//! The window's surface, frame, and appearance follow the console.
//!
//! The console publishes its effective theme as `<meta name="theme-color">`,
//! which WebKit exposes as `WKWebView.themeColor`. The shell observes that
//! property, paints the window in it, and gives the window the dark or light
//! Aqua appearance its luminance implies, so the window behind the page, its
//! title bar, and the alerts and panels the shell opens for the page agree with
//! whatever theme the console chose. The web view draws no background of its
//! own, so before first paint and in the gap a live resize opens, the window's
//! colour shows rather than WebKit's white.
//!
//! The web view itself keeps the system appearance. WebKit answers the page's
//! `prefers-color-scheme` from the web view's appearance, and the console's
//! "system" theme reads exactly that; inheriting the window's appearance would
//! pin the console to the last theme it chose and stop it following macOS. The
//! shell therefore pins the web view to the application's effective appearance,
//! which the shell never overrides, and re-pins it whenever macOS changes it.
//!
//! The last colour any console painted is remembered in the user defaults, so
//! the next launch, for any repository, paints it before the page loads. Each
//! repository keeps its own window frame there, so its window reopens where
//! the operator left it.

use std::ffi::c_void;
use std::ptr::null_mut;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSApplication, NSColor, NSColorSpace, NSScreen, NSWindow,
};
use objc2_foundation::{
    MainThreadMarker, NSKeyValueObservingOptions, NSNumber, NSObjectNSKeyValueCoding,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSRect, NSString, NSUserDefaults,
};
use objc2_web_kit::WKWebView;

/// The observed `WKWebView` property carrying the page's theme colour.
const THEME_COLOR: &str = "themeColor";

/// The observed `NSApplication` property carrying the system appearance.
const EFFECTIVE_APPEARANCE: &str = "effectiveAppearance";

/// Relative luminance below which a theme colour reads as a dark theme.
const DARK_LUMINANCE: f64 = 0.5;

/// The window's surface for as long as the event loop runs; dropping it stops
/// observing the web view and the application.
pub(super) struct Surface {
    observer: Retained<SurfaceObserver>,
}

impl Surface {
    /// Restore `key`'s frame and the remembered colour onto `window` and start
    /// following the page's theme colour in `webview`.
    pub(super) fn attach(
        main_thread: MainThreadMarker,
        window: Retained<NSWindow>,
        webview: Retained<WKWebView>,
        key: &str,
    ) -> Self {
        restore_frame(&window, key);
        let observer = SurfaceObserver::new(main_thread, window, webview);
        let webview = &observer.ivars().webview;
        let application = NSApplication::sharedApplication(main_thread);
        // SAFETY: `drawsBackground` is a Boolean key WKWebView answers on
        // macOS; the value is an NSNumber of the matching type. The observer
        // is a live NSObject implementing
        // `observeValueForKeyPath:ofObject:change:context:`; `Drop` removes
        // both registrations before either object is released, and the
        // context is unused.
        unsafe {
            webview.setValue_forKey(
                Some(&NSNumber::numberWithBool(false)),
                &NSString::from_str("drawsBackground"),
            );
            application.addObserver_forKeyPath_options_context(
                &observer,
                &NSString::from_str(EFFECTIVE_APPEARANCE),
                NSKeyValueObservingOptions::Initial | NSKeyValueObservingOptions::New,
                null_mut(),
            );
        }
        if let Some(color) = observer.remembered_color() {
            observer.paint(&color);
        }
        // SAFETY: as above.
        unsafe {
            webview.addObserver_forKeyPath_options_context(
                &observer,
                &NSString::from_str(THEME_COLOR),
                NSKeyValueObservingOptions::Initial | NSKeyValueObservingOptions::New,
                null_mut(),
            );
        }
        Self { observer }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        let application = NSApplication::sharedApplication(self.observer.mtm());
        // SAFETY: exactly the registrations `attach` made, each removed once.
        unsafe {
            self.observer
                .ivars()
                .webview
                .removeObserver_forKeyPath(&self.observer, &NSString::from_str(THEME_COLOR));
            application.removeObserver_forKeyPath(
                &self.observer,
                &NSString::from_str(EFFECTIVE_APPEARANCE),
            );
        }
    }
}

/// Reopen at `key`'s saved frame and keep saving it there. A frame saved on a
/// display that is no longer connected or rearranged, which AppKit leaves
/// barely touching a screen, opens centred on the main screen instead unless
/// at least half of it lies within one screen's visible area.
fn restore_frame(window: &NSWindow, key: &str) {
    let name = NSString::from_str(&format!("Spice {key}"));
    window.setFrameUsingName(&name);
    window.setFrameAutosaveName(&name);
    let frame = window.frame();
    let area = frame.size.width * frame.size.height;
    let visible = NSScreen::screens(window.mtm())
        .iter()
        .any(|screen| 2.0 * overlap(frame, screen.visibleFrame()) >= area);
    if !visible {
        window.center();
    }
}

/// The area two rectangles share.
fn overlap(a: NSRect, b: NSRect) -> f64 {
    let width =
        (a.origin.x + a.size.width).min(b.origin.x + b.size.width) - a.origin.x.max(b.origin.x);
    let height =
        (a.origin.y + a.size.height).min(b.origin.y + b.size.height) - a.origin.y.max(b.origin.y);
    width.max(0.0) * height.max(0.0)
}

struct SurfaceObserverIvars {
    window: Retained<NSWindow>,
    webview: Retained<WKWebView>,
}

/// The user-defaults key holding the last painted colour as `"r g b"` in sRGB.
fn remembered_key() -> Retained<NSString> {
    NSString::from_str("Spice Theme Color")
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SpiceShellSurfaceObserver"]
    #[thread_kind = MainThreadOnly]
    #[ivars = SurfaceObserverIvars]
    struct SurfaceObserver;

    unsafe impl NSObjectProtocol for SurfaceObserver {}

    impl SurfaceObserver {
        /// AppKit and WebKit report both properties on the main thread. A
        /// page without a theme colour leaves the current surface in place.
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(
            &self,
            key_path: Option<&NSString>,
            _object: Option<&AnyObject>,
            _change: Option<&AnyObject>,
            _context: *mut c_void,
        ) {
            if key_path.is_some_and(|path| path.to_string() == EFFECTIVE_APPEARANCE) {
                self.pin_web_view();
                return;
            }
            // SAFETY: a plain property read on the live web view.
            if let Some(color) = unsafe { self.ivars().webview.themeColor() } {
                self.paint(&color);
                self.remember(&color);
            }
        }
    }
);

impl SurfaceObserver {
    fn new(
        main_thread: MainThreadMarker,
        window: Retained<NSWindow>,
        webview: Retained<WKWebView>,
    ) -> Retained<Self> {
        let observer = main_thread
            .alloc::<Self>()
            .set_ivars(SurfaceObserverIvars { window, webview });
        // SAFETY: plain NSObject init on a freshly allocated instance whose
        // ivars are set.
        unsafe { msg_send![super(observer), init] }
    }

    /// Give the web view the system appearance, which the application's
    /// effective appearance is because the shell never sets the application's.
    fn pin_web_view(&self) {
        let system = NSApplication::sharedApplication(self.mtm()).effectiveAppearance();
        self.ivars().webview.setAppearance(Some(&system));
    }

    /// Paint the window in `color` and give it the appearance its luminance
    /// implies.
    fn paint(&self, color: &NSColor) {
        let Some([red, green, blue]) = srgb(color) else {
            return;
        };
        let window = &self.ivars().window;
        window.setBackgroundColor(Some(color));
        let dark = 0.2126 * red + 0.7152 * green + 0.0722 * blue < DARK_LUMINANCE;
        // SAFETY: both names are AppKit's own appearance constants.
        let name = unsafe {
            if dark {
                NSAppearanceNameDarkAqua
            } else {
                NSAppearanceNameAqua
            }
        };
        window.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
    }

    /// Store `color` as the surface the next launch paints first.
    fn remember(&self, color: &NSColor) {
        let Some([red, green, blue]) = srgb(color) else {
            return;
        };
        let value = NSString::from_str(&format!("{red} {green} {blue}"));
        let defaults = NSUserDefaults::standardUserDefaults();
        // SAFETY: an NSString is a property-list value.
        unsafe { defaults.setObject_forKey(Some(&value), &remembered_key()) };
    }

    /// The colour the last console painted, if any.
    fn remembered_color(&self) -> Option<Retained<NSColor>> {
        let value = NSUserDefaults::standardUserDefaults()
            .stringForKey(&remembered_key())?
            .to_string();
        let mut components = value.split(' ').map(str::parse::<f64>);
        let mut next = || {
            components
                .next()?
                .ok()
                .filter(|value| (0.0..=1.0).contains(value))
        };
        let [red, green, blue] = [next()?, next()?, next()?];
        Some(NSColor::colorWithSRGBRed_green_blue_alpha(
            red, green, blue, 1.0,
        ))
    }
}

/// `color`'s red, green, and blue components in sRGB, or `None` for a colour
/// with no sRGB form, such as a pattern.
fn srgb(color: &NSColor) -> Option<[f64; 3]> {
    let color = color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    Some([
        color.redComponent(),
        color.greenComponent(),
        color.blueComponent(),
    ])
}
