//! Zeble: a PostgreSQL browser.

mod assets;
mod form;
mod grid;
mod object_tab;
mod query_tab;
mod relation_tab;
mod sidebar;
mod launcher;
mod session;
mod util;
mod workspace;

use gpui_kit::*;

actions!(zeble, [Quit, OpenLauncher]);

fn main() {
    let app = gpui_kit::application().with_assets(assets::AppAssets);
    // macOS: clicking the dock icon with no windows brings the list back.
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            launcher::open_launcher(cx);
        }
    });
    app.run(|cx| {
            gpui_kit::init(cx);
            gpui_kit::component::Theme::sync_system_appearance(None, cx);
            launcher::init(cx);
            workspace::init(cx);

            cx.bind_keys([
                KeyBinding::new(util::keys("mod-q"), Quit, None),
                KeyBinding::new(util::keys("mod-n"), OpenLauncher, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_action(|_: &OpenLauncher, cx| launcher::open_launcher(cx));
            cx.set_menus(vec![Menu {
                name: "Zeble".into(),
                items: vec![
                    MenuItem::action("Connections", OpenLauncher),
                    MenuItem::separator(),
                    MenuItem::action("Quit Zeble", Quit),
                ],
                disabled: false,
            }]);
            // Other platforms quit with their last window, macOS keeps running.
            #[cfg(not(target_os = "macos"))]
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            launcher::open_launcher(cx);
            cx.activate(true);
        });
}
