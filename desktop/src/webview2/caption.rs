//! The window's own controls once Windows draws no title bar.
//!
//! The window is undecorated, so the console's header is its title bar (see
//! [`crate::header`]) and Windows draws no caption buttons. The shell puts its
//! own in the header's trailing corner, where Windows keeps them, the way the
//! macOS shell keeps AppKit's traffic lights in the leading corner. They live
//! in a closed shadow root outside the app's tree, so no app style or render
//! reaches them. They take the header's height and the header's ink, and their
//! glyphs come from the system's caption icon font. The shell publishes their
//! width as `--app-titlebar-inset-end` on the document root; the header
//! reserves that much trailing space, so the browser, which never sets it,
//! renders exactly as before.
//!
//! Windows resizes an undecorated window natively from the invisible frame on
//! its left, right, and bottom, but the top edge lies inside the client area,
//! which the webview covers. A thin band along the top reports presses there
//! so the shell can start the system resize; it hides while the window is
//! maximized, when Windows does not resize either.

use tao::event_loop::ControlFlow;
use tao::window::{ResizeDirection, Window};
use wry::WebView;

/// The caption buttons' combined width: three of Windows' 46-pixel caption
/// hit targets.
const CONTROLS_WIDTH: u32 = 3 * 46;

/// The page's side: draw the caption controls and the top resize band, report
/// their presses, and follow the maximized state the shell announces.
pub(super) const SCRIPT: &str = r#"(() => {
  const post = (message) => window.ipc.postMessage(message);
  const glyphs = { minimize: "\uE921", maximize: "\uE922", restore: "\uE923", close: "\uE8BB" };
  let maximized = false;
  let controls, maximize, band, bar, attempts = 0;
  const apply = () => {
    if (!controls) return;
    maximize.textContent = maximized ? glyphs.restore : glyphs.maximize;
    maximize.setAttribute("aria-label", maximized ? "Restore" : "Maximize");
    band.hidden = maximized;
  };
  const sync = () => {
    if (!controls) return;
    if (!bar || !bar.isConnected) {
      bar = document.querySelector(".commandbar");
      if (!bar) {
        if (attempts++ < 600) requestAnimationFrame(sync);
        return;
      }
      attempts = 0;
      new ResizeObserver(sync).observe(bar);
    }
    controls.style.setProperty("--ink", getComputedStyle(bar).color);
    controls.style.setProperty("--height", `${bar.getBoundingClientRect().height}px`);
  };
  const mount = () => {
    const host = document.createElement("spice-window-controls");
    const root = host.attachShadow({ mode: "closed" });
    // The console serves `style-src 'self'`, which refuses a <style> element;
    // a constructed stylesheet is CSSOM, which the policy admits.
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(`
      :host { all: initial; }
      .controls { position: fixed; z-index: 2147483646; top: 0; right: 0; display: flex; height: var(--height, 32px); color: var(--ink, CanvasText); font: 10px "Segoe Fluent Icons", "Segoe MDL2 Assets"; user-select: none; }
      button { all: unset; display: grid; place-items: center; width: 46px; height: 100%; color: inherit; font: inherit; }
      button:hover { background: color-mix(in srgb, currentColor 9%, transparent); }
      button:active { background: color-mix(in srgb, currentColor 16%, transparent); }
      button.close:hover { background: #c42b1c; color: #fff; }
      button.close:active { background: color-mix(in srgb, #c42b1c 82%, #fff); color: #fff; }
      .band { position: fixed; z-index: 2147483647; top: 0; right: 0; left: 0; height: 5px; cursor: ns-resize; }
      .band[hidden] { display: none; }
      .band > div { position: absolute; top: 0; width: 12px; height: 100%; }
      .north-west { left: 0; cursor: nwse-resize; }
      .north-east { right: 0; cursor: nesw-resize; }
    `);
    root.adoptedStyleSheets = [sheet];
    root.innerHTML = `<div class="controls">
      <button class="minimize" tabindex="-1" aria-label="Minimize">${glyphs.minimize}</button>
      <button class="maximize" tabindex="-1"></button>
      <button class="close" tabindex="-1" aria-label="Close">${glyphs.close}</button>
    </div>
    <div class="band"><div class="north-west"></div><div class="north-east"></div></div>`;
    controls = root.querySelector(".controls");
    maximize = root.querySelector(".maximize");
    band = root.querySelector(".band");
    for (const name of ["minimize", "maximize", "close"]) {
      const button = root.querySelector(`.${name}`);
      button.addEventListener("mousedown", (event) => event.preventDefault());
      button.addEventListener("click", () => post(name));
    }
    band.addEventListener("mousedown", (event) => {
      if (event.button !== 0) return;
      event.preventDefault();
      const corner = event.target.className;
      post(corner === "north-west" ? "resize-north-west" : corner === "north-east" ? "resize-north-east" : "resize-north");
    });
    document.body.append(host);
    apply();
    sync();
    new MutationObserver(sync).observe(document.documentElement, {
      subtree: true,
      attributeFilter: ["data-app-effective-theme", "data-base-flavor", "data-base-accent", "data-accent"],
    });
  };
  addEventListener("spice-window", (event) => {
    maximized = event.detail.maximized;
    apply();
  });
  if (document.body) mount();
  else addEventListener("DOMContentLoaded", mount, { once: true });
})();"#;

/// What the page asks of its window through the caption controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Request {
    Minimize,
    /// Maximize, or restore a maximized window.
    Maximize,
    Close,
    /// The top resize band was pressed.
    Resize(ResizeDirection),
}

impl Request {
    /// The request one IPC message names, if any.
    pub(super) fn parse(message: &str) -> Option<Self> {
        match message {
            "minimize" => Some(Self::Minimize),
            "maximize" => Some(Self::Maximize),
            "close" => Some(Self::Close),
            "resize-north" => Some(Self::Resize(ResizeDirection::North)),
            "resize-north-west" => Some(Self::Resize(ResizeDirection::NorthWest)),
            "resize-north-east" => Some(Self::Resize(ResizeDirection::NorthEast)),
            _ => None,
        }
    }
}

/// The maximized state last published to the page.
#[derive(Default)]
pub(super) struct Caption {
    published: Option<bool>,
}

impl Caption {
    /// A document started: publish the inset and state to it afresh.
    pub(super) fn ready(&mut self, window: &Window, webview: &WebView) {
        self.published = None;
        self.follow(window, webview);
    }

    /// Publish the maximized state whenever it changed.
    pub(super) fn follow(&mut self, window: &Window, webview: &WebView) {
        let maximized = window.is_maximized();
        if self.published == Some(maximized) {
            return;
        }
        let script = format!(
            "document.documentElement.style.setProperty('--app-titlebar-inset-end', '{CONTROLS_WIDTH}px');\
             dispatchEvent(new CustomEvent('spice-window', {{ detail: {{ maximized: {maximized} }} }}))"
        );
        if webview.evaluate_script(&script).is_ok() {
            self.published = Some(maximized);
        }
    }
}

/// Answer one caption request; close ends the event loop, as the system close
/// does.
pub(super) fn answer(request: Request, window: &Window) -> ControlFlow {
    match request {
        Request::Minimize => window.set_minimized(true),
        Request::Maximize => toggle_maximized(window),
        Request::Close => return ControlFlow::Exit,
        Request::Resize(direction) => {
            if !window.is_maximized() {
                let _ = window.drag_resize_window(direction);
            }
        }
    }
    ControlFlow::Wait
}

/// Windows' title-bar double-click and maximize-button action.
pub(super) fn toggle_maximized(window: &Window) {
    window.set_maximized(!window.is_maximized());
}
