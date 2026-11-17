// SPDX-License-Identifier: GPL-3.0-or-later
//! The app's window: WebView2 showing the local app's page. Closing it can leave OpenFarm working
//! in the background (the farm and the renders don't depend on the window); opening OpenFarm again
//! brings the window back.

use std::time::Duration;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::WindowExtWindows;
use tao::window::{Icon, WindowBuilder};
use wry::{WebContext, WebViewBuilder};

use crate::local::Control;

pub enum UserEvent {
    Show,
    Quit,
    Restart,
}

/// The window icon: the 256-pixel picture drawn by tools/make_icon.ps1.
fn icon() -> Option<Icon> {
    let picture = image::load_from_memory(include_bytes!("../assets/icon-256.png")).ok()?.into_rgba8();
    let (w, h) = picture.dimensions();
    Icon::from_rgba(picture.into_raw(), w, h).ok()
}

fn ask_keep_running(hwnd: isize) -> Option<bool> {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let text = wide(
        "Keep OpenFarm working in the background?\n\n\
         Yes: close the window. The farm and the renders keep going; open OpenFarm again to bring the window back.\n\
         No: quit OpenFarm on this computer.",
    );
    let caption = wide("OpenFarm");
    const MB_YESNOCANCEL: u32 = 0x3;
    const MB_ICONQUESTION: u32 = 0x20;
    // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the call.
    match unsafe { MessageBoxW(hwnd, text.as_ptr(), caption.as_ptr(), MB_YESNOCANCEL | MB_ICONQUESTION) } {
        6 => Some(true),
        7 => Some(false),
        _ => None,
    }
}

/// Runs the window on this (the main) thread until OpenFarm quits. Returns true to restart.
pub fn run(url: String, data: std::path::PathBuf, mut control: tokio::sync::mpsc::UnboundedReceiver<Control>, ask_on_close: bool) -> ! {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    std::thread::spawn(move || {
        while let Some(c) = control.blocking_recv() {
            let ev = match c {
                Control::Show => UserEvent::Show,
                Control::Quit => UserEvent::Quit,
                Control::Restart => UserEvent::Restart,
            };
            if proxy.send_event(ev).is_err() {
                break;
            }
        }
    });
    let window = WindowBuilder::new()
        .with_title("OpenFarm")
        .with_window_icon(icon())
        .with_min_inner_size(LogicalSize::new(720.0, 520.0))
        .with_inner_size(LogicalSize::new(1280.0, 840.0))
        .build(&event_loop)
        .expect("the window");
    let mut context = WebContext::new(Some(data.join("webview")));
    let base = url.clone();
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(&url)
        .with_devtools(cfg!(debug_assertions))
        .with_navigation_handler(move |target| {
            if target.starts_with(&base) {
                true
            } else {
                let _ = std::process::Command::new("explorer").arg(&target).spawn();
                false
            }
        })
        .with_new_window_req_handler(|target, _| {
            let _ = std::process::Command::new("explorer").arg(&target).spawn();
            wry::NewWindowResponse::Deny
        })
        .build(&window);
    let webview = match webview {
        Ok(w) => w,
        Err(_) => {
            // No WebView2: the default browser shows the same page; the process keeps running.
            let _ = std::process::Command::new("explorer").arg(&url).spawn();
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
    };
    let mut restart = false;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let _keep = &webview;
        match event {
            Event::UserEvent(UserEvent::Show) => {
                window.set_visible(true);
                window.set_minimized(false);
                window.set_focus();
            }
            Event::UserEvent(UserEvent::Quit) => *control_flow = ControlFlow::Exit,
            Event::UserEvent(UserEvent::Restart) => {
                restart = true;
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if !ask_on_close {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                match ask_keep_running(window.hwnd()) {
                    Some(true) => window.set_visible(false),
                    Some(false) => *control_flow = ControlFlow::Exit,
                    None => {}
                }
            }
            Event::LoopDestroyed => {
                if restart {
                    crate::relaunch();
                }
                std::process::exit(0);
            }
            _ => {}
        }
    });
}
