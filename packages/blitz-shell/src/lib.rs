#![cfg_attr(docsrs, feature(doc_cfg))]

//! Event loop, windowing and system integration.
//!
//! ## Feature flags
//!  - `default`: Enables the features listed below.
//!  - `accessibility`: Enables [`accesskit`] accessibility support.
//!  - `hot-reload`: Enables hot-reloading of Dioxus RSX.
//!  - `tracing`: Enables tracing support.

mod application;
mod convert_events;
mod event;
mod net;
mod window;

#[cfg(feature = "accessibility")]
mod accessibility;

pub use crate::application::BlitzApplication;
pub use crate::event::{BlitzShellEvent, BlitzShellProxy};
pub use crate::window::{View, WindowConfig};

#[cfg(feature = "data-uri")]
pub use crate::net::DataUriNetProvider;

#[cfg(all(
    feature = "file-dialog",
    any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
use blitz_traits::shell::FileDialogFilter;
use blitz_traits::shell::ShellProvider;
use std::sync::Arc;
use winit::cursor::{Cursor, CursorIcon};
use winit::dpi::{LogicalPosition, LogicalSize};
pub use winit::event_loop::{ControlFlow, EventLoop, EventLoopProxy};
pub use winit::window::Window;
use winit::window::{ImeCapabilities, ImeEnableRequest, ImeRequest, ImeRequestData};

#[derive(Default)]
pub struct Config {
    pub stylesheets: Vec<String>,
    pub base_url: Option<String>,
}

/// Build an event loop for the application
pub fn create_default_event_loop() -> EventLoop {
    let mut ev_builder = EventLoop::builder();
    #[cfg(target_os = "android")]
    {
        use winit::platform::android::EventLoopBuilderExtAndroid;
        ev_builder.with_android_app(current_android_app());
    }

    let event_loop = ev_builder.build().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);

    event_loop
}

#[cfg(target_os = "android")]
static ANDROID_APP: std::sync::OnceLock<android_activity::AndroidApp> = std::sync::OnceLock::new();

#[cfg(target_os = "android")]
#[cfg_attr(docsrs, doc(cfg(target_os = "android")))]
/// Set the current [`AndroidApp`](android_activity::AndroidApp).
pub fn set_android_app(app: android_activity::AndroidApp) {
    ANDROID_APP.set(app).unwrap()
}

#[cfg(target_os = "android")]
#[cfg_attr(docsrs, doc(cfg(target_os = "android")))]
/// Get the current [`AndroidApp`](android_activity::AndroidApp).
/// This will panic if the android activity has not been setup with [`set_android_app`].
pub fn current_android_app() -> android_activity::AndroidApp {
    ANDROID_APP.get().unwrap().clone()
}

pub struct BlitzShellProvider {
    window: Arc<dyn Window>,
    proxy: BlitzShellProxy,
}
impl BlitzShellProvider {
    pub fn new(window: Arc<dyn Window>, proxy: BlitzShellProxy) -> Self {
        Self { window, proxy }
    }
}

impl ShellProvider for BlitzShellProvider {
    fn request_redraw(&self) {
        self.window.request_redraw();
    }
    fn set_cursor(&self, icon: Option<CursorIcon>) {
        match icon {
            Some(icon) => {
                self.window.set_cursor_visible(true);
                self.window.set_cursor(Cursor::Icon(icon));
            }
            None => {
                self.window.set_cursor(Cursor::Icon(CursorIcon::Default));
                self.window.set_cursor_visible(false)
            }
        }
    }
    fn set_window_title(&self, title: String) {
        self.window.set_title(&title);
    }
    fn set_ime_enabled(&self, is_enabled: bool) {
        if is_enabled {
            let _ = self.window.request_ime_update(ImeRequest::Enable(
                ImeEnableRequest::new(ImeCapabilities::new(), ImeRequestData::default()).unwrap(),
            ));
        } else {
            let _ = self.window.request_ime_update(ImeRequest::Disable);
        }
    }
    fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        let _ = self.window.request_ime_update(ImeRequest::Update(
            ImeRequestData::default().with_cursor_area(
                LogicalPosition::new(x, y).into(),
                LogicalSize::new(width, height).into(),
            ),
        ));
    }

    fn request_window_close(&self) {
        self.proxy.send_event(BlitzShellEvent::CloseWindow {
            window_id: self.window.id(),
        });
    }
    fn set_window_minimized(&self, minimized: bool) {
        self.window.set_minimized(minimized);
    }
    fn set_window_maximized(&self, maximized: bool) {
        self.window.set_maximized(maximized);
    }
    fn is_window_maximized(&self) -> bool {
        self.window.is_maximized()
    }
    fn set_window_decorations(&self, decorations: bool) {
        self.window.set_decorations(decorations);
    }
    fn drag_window(&self) {
        let _ = self.window.drag_window();
    }

    #[cfg(all(
        feature = "clipboard",
        any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        )
    ))]
    fn get_clipboard_text(&self) -> Result<String, blitz_traits::shell::ClipboardError> {
        system_clipboard::read_with(arboard::Clipboard::new)
    }

    #[cfg(all(
        feature = "clipboard",
        any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        )
    ))]
    fn set_clipboard_text(&self, text: String) -> Result<(), blitz_traits::shell::ClipboardError> {
        system_clipboard::write_with(arboard::Clipboard::new, text)
    }

    #[cfg(all(
        feature = "file-dialog",
        any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        )
    ))]
    fn open_file_dialog(
        &self,
        multiple: bool,
        filter: Option<FileDialogFilter>,
    ) -> Vec<std::path::PathBuf> {
        let mut dialog = rfd::FileDialog::new();
        if let Some(FileDialogFilter { name, extensions }) = filter {
            dialog = dialog.add_filter(&name, &extensions);
        }
        let files = if multiple {
            dialog.pick_files()
        } else {
            dialog.pick_file().map(|file| vec![file])
        };
        files.unwrap_or_default()
    }
}

/// The system clipboard, opened for each read or write. Opening fails
/// without a session to serve it, such as native Wayland with no XWayland,
/// since arboard is built here for X11 only. That failure comes back as a
/// `ClipboardError`, like a failed read or write, instead of a panic.
#[cfg(all(
    feature = "clipboard",
    any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
mod system_clipboard {
    use blitz_traits::shell::ClipboardError;

    pub(crate) fn read_with(
        open: impl FnOnce() -> Result<arboard::Clipboard, arboard::Error>,
    ) -> Result<String, ClipboardError> {
        open()
            .and_then(|mut cb| cb.get_text())
            .map_err(|_| ClipboardError)
    }

    pub(crate) fn write_with(
        open: impl FnOnce() -> Result<arboard::Clipboard, arboard::Error>,
        text: String,
    ) -> Result<(), ClipboardError> {
        open()
            .and_then(|mut cb| cb.set_text(text))
            .map_err(|_| ClipboardError)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn unavailable() -> Result<arboard::Clipboard, arboard::Error> {
            Err(arboard::Error::ClipboardNotSupported)
        }

        #[test]
        fn a_clipboard_that_cannot_open_is_an_error_for_reads_and_writes() {
            assert!(read_with(unavailable).is_err());
            assert!(write_with(unavailable, "text".into()).is_err());
        }

        /// The real constructor with no X11 or Wayland display to reach. It
        /// runs in a child process with those variables removed, so this
        /// process's environment is never changed.
        #[cfg(target_os = "linux")]
        #[test]
        fn without_a_display_the_real_clipboard_is_an_error_not_a_panic() {
            const CHILD: &str = "BLITZ_SHELL_CLIPBOARD_CHILD";
            const NAME: &str = "system_clipboard::tests::without_a_display_the_real_clipboard_is_an_error_not_a_panic";
            if std::env::var_os(CHILD).is_some() {
                assert!(read_with(arboard::Clipboard::new).is_err());
                assert!(write_with(arboard::Clipboard::new, "text".into()).is_err());
                return;
            }
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
                .env(CHILD, "1")
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .output()
                .unwrap();
            let log = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "child failed:\n{log}");
            assert!(log.contains("1 passed"), "child ran no test:\n{log}");
        }
    }
}
