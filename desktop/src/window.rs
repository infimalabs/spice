//! The macOS window: one `tao` event loop hosting one `wry` webview.
//!
//! `wry`'s own `WKUIDelegate` answers file-upload panels and new-window
//! requests but leaves JavaScript `alert` and `confirm` unanswered, which
//! WebKit then settles as dismissed: every `confirm` returns `false`. The
//! console gates task settlement and draft discard on `confirm`, so the shell
//! installs its own delegate that puts those dialogs on screen as native
//! alerts, keeps the file panel, and answers new-window requests through the
//! console's link policy (see [`crate::link`]), which also filters every
//! navigation.
//!
//! A process without a main menu has no key equivalents: Cmd+C, Cmd+V, and
//! Cmd+Q do nothing in its window. The shell installs the standard menu bar
//! before the event loop runs. Editing items send the ordinary responder
//! actions, which the webview answers for its focused field; Quit and Close
//! ask the window to close, so they end the event loop exactly as its close
//! button does and the command still settles the resident afterwards.
//! Settings (Cmd+,) opens the console's Spice panel: the shell clicks the
//! command bar's Spice launcher, so the panel opens exactly as a click does.
//!
//! The process showing the window must carry a bundle identity (see
//! [`crate::bundle`]), or macOS dictation never attaches to its fields; the
//! Edit menu's Start Dictation item sends the ordinary responder action.
//!
//! The console's header is the window's title bar (see [`titlebar`]), and the
//! console speaks through the shell's own synthesizer (see [`speech`]).

mod speech;
mod surface;
mod titlebar;

use std::ptr::null_mut;
use std::sync::Arc;

use block2::Block;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAppearance, NSAppearanceCustomization, NSApplication,
    NSMenu, NSMenuItem, NSModalResponseOK, NSOpenPanel, NSWindow,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSObjectProtocol, NSString, NSURL};
use objc2_web_kit::{
    WKFrameInfo, WKNavigationAction, WKOpenPanelParameters, WKUIDelegate, WKWebView,
    WKWebViewConfiguration, WKWindowFeatures,
};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::macos::{WindowBuilderExtMacOS, WindowExtMacOS};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;
use wry::{WebViewBuilder, WebViewExtMacOS};

use crate::link::Links;
use crate::{INITIAL_SIZE, WindowError};

/// What reaches the window's event loop from the page, the synthesizer, and
/// the menu bar.
enum ShellEvent {
    Titlebar(crate::header::Request),
    Speech(speech::Event),
    /// The Settings menu item: open the console's Spice panel.
    OpenSettings,
}

/// The console's Spice launcher, which [`ShellEvent::OpenSettings`] clicks;
/// the app marks the command bar's launcher, not the open panel's close
/// control, with this focus key. Opening is idempotent while the panel shows.
const OPEN_SETTINGS_SCRIPT: &str =
    r#"document.querySelector('.app-spice-button[data-sidebar-focus-key="spice"]')?.click()"#;

pub(crate) fn show(
    title: &str,
    key: &str,
    url: &str,
    external: impl Fn(&str) + Send + Sync + 'static,
    opened: impl FnOnce(),
) -> Result<(), WindowError> {
    let mut event_loop = EventLoopBuilder::<ShellEvent>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(title)
        .with_inner_size(INITIAL_SIZE)
        .with_titlebar_transparent(true)
        .with_title_hidden(true)
        .with_fullsize_content_view(true)
        .build(&event_loop)
        .map_err(|error| WindowError::Platform(error.to_string()))?;
    let proxy = event_loop.create_proxy();
    let links = Arc::new(Links::new(url, external));
    let navigation = Arc::clone(&links);
    let webview = WebViewBuilder::new()
        .with_url(url)
        .with_navigation_handler(move |target| navigation.navigate(&target))
        .with_initialization_script(crate::header::SCRIPT)
        .with_initialization_script(speech::SCRIPT)
        .with_ipc_handler(move |request| {
            let body = request.body();
            let event = crate::header::Request::parse(body)
                .map(ShellEvent::Titlebar)
                .or_else(|| speech::Event::parse(body).map(ShellEvent::Speech));
            if let Some(event) = event {
                let _ = proxy.send_event(event);
            }
        })
        .build(&window)
        .map_err(|error| WindowError::Platform(error.to_string()))?;
    let main_thread = MainThreadMarker::new()
        .ok_or_else(|| WindowError::Platform("the window must open on the main thread".into()))?;
    // WebKit holds its UI delegate weakly; this binding keeps ours alive for
    // the window's whole life.
    let delegate = ShellDelegate::new(main_thread, links);
    // SAFETY: the delegate outlives the webview (dropped after the event loop
    // returns), and this runs on the main thread the webview was built on.
    unsafe {
        webview
            .webview()
            .setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    }
    // SAFETY: `ns_window` is the window's live NSWindow until the window is
    // dropped, which happens after the event loop returns; retaining it here
    // keeps the menu target's reference valid for exactly that span.
    let ns_window = unsafe { Retained::retain(window.ns_window().cast::<NSWindow>()) }
        .ok_or_else(|| WindowError::Platform("the window has no NSWindow".into()))?;
    let menu_target = MenuTarget::new(main_thread, ns_window.clone(), event_loop.create_proxy());
    install_main_menu(main_thread, &menu_target);
    let surface = surface::Surface::attach(
        main_thread,
        ns_window.clone(),
        Retained::into_super(webview.webview()),
        key,
    );
    let reporter = event_loop.create_proxy();
    let mut speech = speech::Speech::new(move |event| {
        let _ = reporter.send_event(ShellEvent::Speech(event));
    });
    let mut titlebar = titlebar::Titlebar::default();
    titlebar.follow(&window, &ns_window, &webview);
    opened();
    event_loop.run_return(|event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            Event::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            } => titlebar.follow(&window, &ns_window, &webview),
            Event::UserEvent(ShellEvent::Titlebar(request)) => {
                titlebar.answer(request, &window, &ns_window, &webview);
            }
            Event::UserEvent(ShellEvent::Speech(event)) => speech.answer(event, &webview),
            Event::UserEvent(ShellEvent::OpenSettings) => {
                let _ = webview.evaluate_script(OPEN_SETTINGS_SCRIPT);
            }
            _ => {}
        }
    });
    drop(speech);
    drop(surface);
    drop(delegate);
    NSApplication::sharedApplication(main_thread).setMainMenu(None);
    drop(menu_target);
    Ok(())
}

/// The standard menu bar: the application menu with Settings, Hide, and
/// Quit, Edit with the responder-chain editing actions, and Window with
/// Minimize and Close.
///
/// Quit and Close are not `terminate:`: they target the shell's own window
/// close, so `spice app` regains control after the window goes and settles
/// the resident it started.
fn install_main_menu(main_thread: MainThreadMarker, target: &MenuTarget) {
    let menu = NSMenu::new(main_thread);
    let application = NSMenu::new(main_thread);
    application.addItem(&menu_item(
        main_thread,
        "Settings\u{2026}",
        sel!(openSettings:),
        ",",
        Some(target),
    ));
    application.addItem(&NSMenuItem::separatorItem(main_thread));
    application.addItem(&menu_item(
        main_thread,
        "Hide Spice",
        sel!(hide:),
        "h",
        None,
    ));
    application.addItem(&NSMenuItem::separatorItem(main_thread));
    application.addItem(&menu_item(
        main_thread,
        "Quit Spice",
        sel!(closeWindow:),
        "q",
        Some(target),
    ));
    menu.addItem(&submenu_item(main_thread, "Spice", &application));

    let edit = NSMenu::initWithTitle(NSMenu::alloc(main_thread), &NSString::from_str("Edit"));
    for (title, action, key) in [
        ("Undo", sel!(undo:), "z"),
        ("Redo", sel!(redo:), "Z"),
        ("Cut", sel!(cut:), "x"),
        ("Copy", sel!(copy:), "c"),
        ("Paste", sel!(paste:), "v"),
        ("Select All", sel!(selectAll:), "a"),
    ] {
        edit.addItem(&menu_item(main_thread, title, action, key, None));
    }
    edit.addItem(&NSMenuItem::separatorItem(main_thread));
    // The system annotates this item with the dictation shortcut it owns.
    edit.addItem(&menu_item(
        main_thread,
        "Start Dictation\u{2026}",
        sel!(startDictation:),
        "",
        None,
    ));
    menu.addItem(&submenu_item(main_thread, "Edit", &edit));

    let window = NSMenu::initWithTitle(NSMenu::alloc(main_thread), &NSString::from_str("Window"));
    window.addItem(&menu_item(
        main_thread,
        "Minimize",
        sel!(performMiniaturize:),
        "m",
        None,
    ));
    window.addItem(&menu_item(
        main_thread,
        "Close",
        sel!(closeWindow:),
        "w",
        Some(target),
    ));
    menu.addItem(&submenu_item(main_thread, "Window", &window));

    let app = NSApplication::sharedApplication(main_thread);
    app.setWindowsMenu(Some(&window));
    app.setMainMenu(Some(&menu));
}

/// One menu item sending `action` to `target`, or down the responder chain
/// when there is none; `key` with Command is its key equivalent, an uppercase
/// letter adding Shift.
fn menu_item(
    main_thread: MainThreadMarker,
    title: &str,
    action: Sel,
    key: &str,
    target: Option<&MenuTarget>,
) -> Retained<NSMenuItem> {
    // SAFETY: every selector named here is an AppKit responder action or one
    // `MenuTarget` defines; the target, when set, outlives the menu.
    unsafe {
        let item = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(main_thread),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        );
        if let Some(target) = target {
            let target: &AnyObject = target;
            item.setTarget(Some(target));
        }
        item
    }
}

/// The menu-bar item that opens `submenu`.
fn submenu_item(
    main_thread: MainThreadMarker,
    title: &str,
    submenu: &NSMenu,
) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(main_thread);
    item.setTitle(&NSString::from_str(title));
    item.setSubmenu(Some(submenu));
    item
}

struct MenuTargetIvars {
    window: Retained<NSWindow>,
    events: EventLoopProxy<ShellEvent>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SpiceShellMenuTarget"]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuTargetIvars]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        /// Close the shell window the way its close button does: `tao`
        /// answers `windowShouldClose:` with a `CloseRequested` event and the
        /// event loop exits on it.
        #[unsafe(method(closeWindow:))]
        fn close_window(&self, _sender: Option<&AnyObject>) {
            self.ivars().window.performClose(None);
        }

        /// Ask the event loop to open the console's Spice panel.
        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().events.send_event(ShellEvent::OpenSettings);
        }
    }
);

impl MenuTarget {
    fn new(
        main_thread: MainThreadMarker,
        window: Retained<NSWindow>,
        events: EventLoopProxy<ShellEvent>,
    ) -> Retained<Self> {
        let target = main_thread
            .alloc::<Self>()
            .set_ivars(MenuTargetIvars { window, events });
        // SAFETY: plain NSObject init on a freshly allocated instance whose
        // ivars are set.
        unsafe { msg_send![super(target), init] }
    }
}

struct ShellDelegateIvars {
    links: Arc<Links>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SpiceShellUIDelegate"]
    #[thread_kind = MainThreadOnly]
    #[ivars = ShellDelegateIvars]
    struct ShellDelegate;

    unsafe impl NSObjectProtocol for ShellDelegate {}

    unsafe impl WKUIDelegate for ShellDelegate {
        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn alert(
            &self,
            webview: &WKWebView,
            message: &NSString,
            _frame: &WKFrameInfo,
            handler: &Block<dyn Fn()>,
        ) {
            let alert = NSAlert::new(self.mtm());
            alert
                .window()
                .setAppearance(window_appearance(webview).as_deref());
            alert.setMessageText(message);
            alert.addButtonWithTitle(&NSString::from_str("OK"));
            alert.runModal();
            handler.call(());
        }

        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn confirm(
            &self,
            webview: &WKWebView,
            message: &NSString,
            _frame: &WKFrameInfo,
            handler: &Block<dyn Fn(Bool)>,
        ) {
            let alert = NSAlert::new(self.mtm());
            alert
                .window()
                .setAppearance(window_appearance(webview).as_deref());
            alert.setMessageText(message);
            alert.addButtonWithTitle(&NSString::from_str("OK"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            let confirmed = alert.runModal() == NSAlertFirstButtonReturn;
            handler.call((Bool::new(confirmed),));
        }

        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        fn open_panel(
            &self,
            webview: &WKWebView,
            parameters: &WKOpenPanelParameters,
            _frame: &WKFrameInfo,
            handler: &Block<dyn Fn(*const NSArray<NSURL>)>,
        ) {
            let panel = NSOpenPanel::openPanel(self.mtm());
            panel.setAppearance(window_appearance(webview).as_deref());
            panel.setCanChooseFiles(true);
            // SAFETY: plain property reads on a live panel-parameters object
            // WebKit handed to this call.
            unsafe {
                panel.setAllowsMultipleSelection(parameters.allowsMultipleSelection());
                panel.setCanChooseDirectories(parameters.allowsDirectories());
            }
            if panel.runModal() == NSModalResponseOK {
                handler.call((Retained::as_ptr(&panel.URLs()),));
            } else {
                handler.call((null_mut(),));
            }
        }

        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn new_window(
            &self,
            _webview: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            // SAFETY: the navigation action is live for this call.
            let url = unsafe { action.request() }
                .URL()
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string());
            if let Some(url) = url {
                self.ivars().links.new_window(&url);
            }
            None
        }
    }
);

impl ShellDelegate {
    fn new(main_thread: MainThreadMarker, links: Arc<Links>) -> Retained<Self> {
        let delegate = main_thread
            .alloc::<Self>()
            .set_ivars(ShellDelegateIvars { links });
        // SAFETY: plain NSObject init on a freshly allocated instance whose
        // ivars are set.
        unsafe { msg_send![super(delegate), init] }
    }
}

/// The appearance the console's theme gave the shell window, for the alerts
/// and panels the shell opens on its behalf; they are separate windows and
/// would otherwise take the system's.
fn window_appearance(webview: &WKWebView) -> Option<Retained<NSAppearance>> {
    webview.window()?.appearance()
}
