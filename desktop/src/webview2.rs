//! The Windows window: one `tao` event loop hosting one `wry` WebView2
//! webview.
//!
//! The window draws no title bar: the console's header is its title bar (see
//! [`crate::header`]), and the shell's caption controls stand in its trailing
//! corner (see [`caption`]). Windows keeps the window's shadow, its resize
//! frame, and Aero Snap; the shell asks for the rounded corners and border
//! Windows 11 gives captioned windows on its own.
//!
//! WebView2 answers JavaScript dialogs and file pickers itself, so the shell
//! adds only the console's link policy (see [`crate::link`]): navigations
//! away from the console and every new-window request leave through
//! `external`, and the window stays the only webview. A missing WebView2
//! runtime surfaces as the webview build's error.

mod caption;

use std::sync::Arc;

use tao::dpi::{PhysicalPosition, PhysicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::platform::windows::{MonitorHandleExtWindows, WindowBuilderExtWindows, WindowExtWindows};
use tao::window::{Window, WindowBuilder};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWM_WINDOW_CORNER_PREFERENCE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
use wry::{NewWindowResponse, WebViewBuilder};

use crate::header;
use crate::link::Links;
use crate::{INITIAL_SIZE, WindowError};

/// What the page asks of the shell.
enum ShellEvent {
    Header(header::Request),
    Caption(caption::Request),
}

pub(crate) fn show(
    title: &str,
    url: &str,
    external: impl Fn(&str) + Send + Sync + 'static,
    opened: impl FnOnce(),
) -> Result<(), WindowError> {
    let mut event_loop = EventLoopBuilder::<ShellEvent>::with_user_event().build();
    let window = place(WindowBuilder::new(), &event_loop)
        .with_title(title)
        .with_decorations(false)
        .with_undecorated_shadow(true)
        .build(&event_loop)
        .map_err(|error| WindowError::Platform(error.to_string()))?;
    round_corners(&window);
    let proxy = event_loop.create_proxy();
    let links = Arc::new(Links::new(url, external));
    let navigation = Arc::clone(&links);
    let webview = WebViewBuilder::new()
        .with_url(url)
        .with_navigation_handler(move |target| navigation.navigate(&target))
        .with_initialization_script(header::SCRIPT)
        .with_initialization_script(caption::SCRIPT)
        .with_ipc_handler(move |request| {
            let body = request.body();
            let event = header::Request::parse(body)
                .map(ShellEvent::Header)
                .or_else(|| caption::Request::parse(body).map(ShellEvent::Caption));
            if let Some(event) = event {
                let _ = proxy.send_event(event);
            }
        })
        .with_new_window_req_handler(move |link, _| {
            links.new_window(&link);
            NewWindowResponse::Deny
        })
        .build(&window)
        .map_err(|error| WindowError::Platform(error.to_string()))?;
    let mut controls = caption::Caption::default();
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
            } => controls.follow(&window, &webview),
            Event::UserEvent(ShellEvent::Header(request)) => match request {
                header::Request::Ready => controls.ready(&window, &webview),
                header::Request::Drag => {
                    let _ = window.drag_window();
                }
                header::Request::DoubleClick => caption::toggle_maximized(&window),
            },
            Event::UserEvent(ShellEvent::Caption(request)) => {
                *control_flow = caption::answer(request, &window);
            }
            _ => {}
        }
    });
    drop(webview);
    Ok(())
}

/// Place the window at the shared initial size, shrunk to fit a smaller
/// screen, centered in the primary monitor's work area. Windows, unlike
/// AppKit, opens a window larger than its screen and under the taskbar.
fn place(builder: WindowBuilder, event_loop: &EventLoop<ShellEvent>) -> WindowBuilder {
    let Some(monitor) = event_loop.primary_monitor() else {
        return builder.with_inner_size(INITIAL_SIZE);
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };
    // SAFETY: the handle names a live monitor, and `info` is a MONITORINFO
    // whose size field is set, as the call requires.
    if !unsafe { GetMonitorInfoW(HMONITOR(monitor.hmonitor() as _), &raw mut info) }.as_bool() {
        return builder.with_inner_size(INITIAL_SIZE);
    }
    let work = info.rcWork;
    let (width, height) = (
        f64::from(work.right - work.left),
        f64::from(work.bottom - work.top),
    );
    let wanted: PhysicalSize<f64> = INITIAL_SIZE.to_physical(monitor.scale_factor());
    let size = PhysicalSize::new(
        wanted.width.min(width * 0.9),
        wanted.height.min(height * 0.9),
    );
    let position = PhysicalPosition::new(
        f64::from(work.left) + (width - size.width) / 2.0,
        f64::from(work.top) + (height - size.height) / 2.0,
    );
    builder.with_inner_size(size).with_position(position)
}

/// Windows 11 rounds a window's corners and draws its border only when it has
/// a caption, unless asked. Windows 10 refuses the attribute and keeps square
/// corners.
fn round_corners(window: &Window) {
    let preference = DWMWCP_ROUND;
    // SAFETY: the handle is the live window's, and the attribute's value is
    // one DWM_WINDOW_CORNER_PREFERENCE read for exactly its size.
    let _ = unsafe {
        DwmSetWindowAttribute(
            HWND(window.hwnd() as _),
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const preference).cast(),
            size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        )
    };
}
