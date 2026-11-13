//! The console's header is the window's title bar (see [`crate::header`]).
//!
//! The window paints the console under a transparent, titleless title bar, and
//! its traffic lights keep AppKit's standard placement in the window's top
//! leading corner, where Firefox's and iTerm's stand. The shell publishes the
//! width they occupy as `--app-titlebar-inset-start` on the document root; the
//! header reserves that much leading space, so the browser, which never sets it,
//! renders exactly as before. Full screen hides the traffic lights, so the
//! shell withdraws the inset there. A header double-click performs the
//! system's title-bar double-click action.

use objc2_app_kit::{NSWindow, NSWindowButton};
use objc2_foundation::{NSString, NSUserDefaults};
use tao::window::Window;
use wry::WebView;

use crate::header::Request;

/// The inset last published to the page.
#[derive(Default)]
pub(super) struct Titlebar {
    published: Option<bool>,
}

impl Titlebar {
    /// Answer one page request.
    pub(super) fn answer(
        &mut self,
        request: Request,
        window: &Window,
        ns_window: &NSWindow,
        webview: &WebView,
    ) {
        match request {
            Request::Ready => {
                self.published = None;
                self.follow(window, ns_window, webview);
            }
            Request::Drag => {
                let _ = window.drag_window();
            }
            Request::DoubleClick => double_click(ns_window),
        }
    }

    /// Publish the inset when full screen hid or restored the traffic lights.
    pub(super) fn follow(&mut self, window: &Window, ns_window: &NSWindow, webview: &WebView) {
        let lights = window.fullscreen().is_none();
        if self.published == Some(lights) {
            return;
        }
        let script = match lights.then(|| reserve(ns_window)).flatten() {
            Some(width) => format!(
                "document.documentElement.style.setProperty('--app-titlebar-inset-start', '{width}px')"
            ),
            None => {
                "document.documentElement.style.removeProperty('--app-titlebar-inset-start')".into()
            }
        };
        if webview.evaluate_script(&script).is_ok() {
            self.published = Some(lights);
        }
    }
}

/// The leading width the traffic lights take: the close light's leading inset
/// on both sides of their span.
fn reserve(window: &NSWindow) -> Option<f64> {
    let close = window
        .standardWindowButton(NSWindowButton::CloseButton)?
        .frame();
    let zoom = window
        .standardWindowButton(NSWindowButton::ZoomButton)?
        .frame();
    Some((close.origin.x + zoom.origin.x + zoom.size.width).ceil())
}

/// The user's choice for a title-bar double-click: minimize, nothing, or zoom,
/// which is also the system default.
fn double_click(window: &NSWindow) {
    let action = NSUserDefaults::standardUserDefaults()
        .stringForKey(&NSString::from_str("AppleActionOnDoubleClick"))
        .map(|action| action.to_string());
    match action.as_deref() {
        Some("Minimize") => window.miniaturize(None),
        Some("None") => {}
        _ => window.zoom(None),
    }
}
