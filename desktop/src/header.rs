//! The console's header acts as the window's title bar.
//!
//! The webview takes every click over the page, so the window's own title-bar
//! drag never sees one. The page's initialization script reports a primary
//! mouse-down on the header's empty area, anything outside its controls, over
//! the IPC channel; the shell starts a native window drag for it, or performs
//! the platform's title-bar double-click action for the second press. Each
//! document also announces itself, so the shell republishes what the page
//! needs to reserve around the window's own controls.

/// The page's side: report the header's empty-area presses and announce each
/// document once its DOM exists, including after every reload. An answer to
/// an earlier announcement could reach a document still without a root
/// element, or the outgoing one, and be lost.
pub(crate) const SCRIPT: &str = r#"(() => {
  const controls = "button, a, input, select, textarea, [tabindex], [contenteditable]";
  addEventListener("mousedown", (event) => {
    const target = event.target;
    if (event.button !== 0 || !(target instanceof Element)) return;
    if (!target.closest(".commandbar") || target.closest(controls)) return;
    event.preventDefault();
    window.ipc.postMessage(event.detail === 2 ? "double-click" : "drag");
  });
  const ready = () => window.ipc.postMessage("ready");
  if (document.readyState === "loading") addEventListener("DOMContentLoaded", ready, { once: true });
  else ready();
})();"#;

/// What the page asks of its window through the header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Request {
    /// A document started; publish the current insets to it.
    Ready,
    /// The header's empty area was pressed: move the window.
    Drag,
    /// The header's empty area was pressed twice.
    DoubleClick,
}

impl Request {
    /// The request one IPC message names, if any.
    pub(crate) fn parse(message: &str) -> Option<Self> {
        match message {
            "ready" => Some(Self::Ready),
            "drag" => Some(Self::Drag),
            "double-click" => Some(Self::DoubleClick),
            _ => None,
        }
    }
}
