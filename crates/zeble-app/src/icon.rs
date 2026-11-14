//! The app icon, rendered from `icon.svg` by `scripts/render-icon.sh`.
//!
//! A bundled `Zeble.app` gets its icon from `AppIcon.icns` in the bundle.
//! These cover running the plain binary (`cargo run`): the Dock icon on
//! macOS and the window icon on X11.

use std::sync::{Arc, OnceLock};

const ICON_PNG: &[u8] = include_bytes!("../../../assets/icon/icon-512.png");
/// Window icons are sent to the X server in one property; 256 px is plenty
/// for window managers and keeps the request small.
const WINDOW_ICON_PNG: &[u8] = include_bytes!("../../../assets/icon/icon-256.png");

/// The icon as RGBA, for `WindowOptions::icon` (used on X11 only).
pub fn window_icon() -> Option<Arc<image::RgbaImage>> {
    static ICON: OnceLock<Option<Arc<image::RgbaImage>>> = OnceLock::new();
    ICON.get_or_init(|| {
        image::load_from_memory_with_format(WINDOW_ICON_PNG, image::ImageFormat::Png)
            .ok()
            .map(|img| Arc::new(img.into_rgba8()))
    })
    .clone()
}

/// Show the icon in the Dock. Call on the main thread after the app starts.
#[cfg(target_os = "macos")]
pub fn set_dock_icon() {
    use objc2::{AnyThread as _, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(ICON_PNG);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        // SAFETY: a valid image on the main thread.
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon() {}

#[cfg(test)]
mod tests {
    #[::core::prelude::v1::test]
    fn icon_decodes() {
        let icon = super::window_icon().expect("icon PNG decodes");
        assert_eq!(icon.dimensions(), (256, 256));
    }
}
