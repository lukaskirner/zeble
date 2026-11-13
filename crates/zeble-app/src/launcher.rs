//! The start window: saved connections (drag to reorder) and the form to add
//! or edit one. Double-clicking a connection opens it in a new window.

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    label::Label,
    menu::{ContextMenuExt as _, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeble_core::{ConnectionConfig, store::ConnectionStore};

use crate::{
    form::{ConnectionForm, FormEvent},
    util::color_tag,
};

/// The saved connections, shared by every window.
pub struct Connections {
    pub store: ConnectionStore,
    pub load_error: Option<SharedString>,
}

struct GlobalConnections(Entity<Connections>);
impl Global for GlobalConnections {}

pub fn init(cx: &mut App) {
    let (store, load_error) = match ConnectionStore::load_default() {
        Ok(s) => (s, None),
        Err(e) => (
            // Keep the broken file untouched: saves go to a side file.
            ConnectionStore::load(zeble_core::store::data_dir().join("connections.recovered.json"))
                .expect("empty store"),
            Some(format!("Could not read saved connections: {e:#}").into()),
        ),
    };
    let entity = cx.new(|_| Connections { store, load_error });
    cx.set_global(GlobalConnections(entity));
}

pub fn connections(cx: &App) -> Entity<Connections> {
    cx.global::<GlobalConnections>().0.clone()
}

/// The launcher window, if one is open.
struct LauncherWindow(Option<WindowHandle<gpui_kit::component::Root>>);
impl Global for LauncherWindow {}

/// Open the connection list, or bring it to the front if it is open.
pub fn open_launcher(cx: &mut App) {
    if let Some(LauncherWindow(Some(handle))) = cx.try_global::<LauncherWindow>() {
        let handle = *handle;
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(920.), px(640.)), cx)),
        window_min_size: Some(size(px(700.), px(480.))),
        titlebar: Some(TitlebarOptions {
            title: Some("Zeble".into()),
            ..Default::default()
        }),
        app_id: Some("zeble".into()),
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Launcher::new(window, cx))
    }) {
        Ok((handle, _)) => {
            cx.set_global(LauncherWindow(
                handle.downcast::<gpui_kit::component::Root>(),
            ));
        }
        Err(e) => eprintln!("failed to open window: {e:#}"),
    }
}

#[derive(Clone)]
struct DraggedConnection {
    ix: usize,
    name: SharedString,
}

impl Render for DraggedConnection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_md()
            .text_sm()
            .child(self.name.clone())
    }
}

pub struct Launcher {
    connections: Entity<Connections>,
    selected: Option<String>,
    form: Option<Entity<ConnectionForm>>,
    _subscriptions: Vec<Subscription>,
    form_subscription: Option<Subscription>,
}

impl Launcher {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let connections = connections(cx);
        let subscriptions = vec![cx.observe(&connections, |_, _, cx| cx.notify())];
        window
            .observe_window_appearance(|window, cx| {
                gpui_kit::component::Theme::sync_system_appearance(Some(window), cx)
            })
            .detach();
        let mut this = Self {
            connections,
            selected: None,
            form: None,
            _subscriptions: subscriptions,
            form_subscription: None,
        };
        if this.connections.read(cx).store.connections().is_empty() {
            this.new_connection(window, cx);
        }
        this
    }

    fn show_form(
        &mut self,
        config: ConnectionConfig,
        is_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = cx.new(|cx| ConnectionForm::new(config, is_new, window, cx));
        self.form_subscription = Some(cx.subscribe_in(
            &form,
            window,
            |this, _, ev: &FormEvent, window, cx| match ev {
                FormEvent::Saved(config) => {
                    let config = config.clone();
                    let result = this.connections.update(cx, |c, cx| {
                        let r = c.store.upsert(config.clone());
                        cx.notify();
                        r
                    });
                    match result {
                        Ok(()) => {
                            this.selected = Some(config.id.clone());
                            window.push_notification("Connection saved", cx);
                        }
                        Err(e) => window.push_notification(
                            format!("Save failed: {}", zeble_core::db::error_text(&e)),
                            cx,
                        ),
                    }
                    cx.notify();
                }
                FormEvent::Cancelled => {
                    this.form = None;
                    this.form_subscription = None;
                    cx.notify();
                }
            },
        ));
        form.update(cx, |f, cx| f.focus(window, cx));
        self.form = Some(form);
        cx.notify();
    }

    fn new_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = None;
        self.show_form(ConnectionConfig::default(), true, window, cx);
    }

    fn select(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(id.to_string());
        let already = self.form.as_ref().is_some_and(|f| f.read(cx).id() == id);
        if !already && let Some(config) = self.connections.read(cx).store.get(id).cloned() {
            self.show_form(config, false, window, cx);
        }
        cx.notify();
    }

    fn open(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(config) = self.connections.read(cx).store.get(id).cloned() {
            crate::workspace::open_connection_window(config, cx);
        }
    }

    fn duplicate(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.connections.update(cx, |c, cx| {
            let r = c.store.duplicate(id);
            cx.notify();
            r
        });
        match result {
            Ok(Some(copy)) => self.select(&copy.id, window, cx),
            Ok(None) => {}
            Err(e) => window.push_notification(
                format!("Duplicate failed: {}", zeble_core::db::error_text(&e)),
                cx,
            ),
        }
    }

    fn confirm_delete(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.connections.read(cx).store.get(id).cloned() else {
            return;
        };
        let this = cx.entity().downgrade();
        let id = id.to_string();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let id = id.clone();
            alert
                .title(format!("Delete \"{}\"?", config.display_name()))
                .description(
                    "The saved connection and its query tabs are removed. This cannot be undone.",
                )
                .confirm()
                .ok_text("Delete")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let id = id.clone();
                    this.update(cx, |this, cx| {
                        let result = this.connections.update(cx, |c, cx| {
                            let r = c.store.delete(&id);
                            cx.notify();
                            r
                        });
                        if let Err(e) = result {
                            window.push_notification(
                                format!("Delete failed: {}", zeble_core::db::error_text(&e)),
                                cx,
                            );
                        }
                        if this.selected.as_deref() == Some(&id) {
                            this.selected = None;
                        }
                        if this.form.as_ref().is_some_and(|f| f.read(cx).id() == id) {
                            this.form = None;
                        }
                        cx.notify();
                    })
                    .ok();
                    true
                })
        });
    }

    fn render_row(
        &self,
        ix: usize,
        config: &ConnectionConfig,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let selected = self.selected.as_deref() == Some(config.id.as_str());
        let id = config.id.clone();
        let name: SharedString = config.display_name().into();
        let entity = cx.entity().downgrade();
        div()
            .id(SharedString::from(format!("conn-{}", config.id)))
            .w_full()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .cursor_pointer()
            .when(selected, |d| d.bg(theme.list_active))
            .when(!selected, |d| d.hover(|d| d.bg(theme.list_hover)))
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .size_3()
                            .flex_none()
                            .rounded_full()
                            .map(|d| match config.color {
                                Some(c) => d.bg(color_tag(c)),
                                None => d.border_1().border_color(theme.muted_foreground),
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Label::new(name.clone()).text_sm().font_medium().truncate(),
                                    )
                                    .when(config.read_only, |d| {
                                        d.child(
                                            Icon::new(IconName::Eye)
                                                .xsmall()
                                                .text_color(theme.muted_foreground),
                                        )
                                    }),
                            )
                            .child(
                                Label::new(config.summary())
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate(),
                            ),
                    ),
            )
            .on_click(cx.listener({
                let id = id.clone();
                move |this, ev: &ClickEvent, window, cx| {
                    this.select(&id, window, cx);
                    if ev.click_count() >= 2 {
                        this.open(&id, cx);
                    }
                }
            }))
            .on_drag(
                DraggedConnection {
                    ix,
                    name: name.clone(),
                },
                |drag, _, _, cx| cx.new(|_| drag.clone()),
            )
            .drag_over::<DraggedConnection>(move |d, dragged, _, cx| {
                if dragged.ix == ix {
                    d
                } else if dragged.ix > ix {
                    d.border_t_2().border_color(cx.theme().drag_border)
                } else {
                    d.border_b_2().border_color(cx.theme().drag_border)
                }
            })
            .on_drop(
                cx.listener(move |this, dragged: &DraggedConnection, window, cx| {
                    let result = this.connections.update(cx, |c, cx| {
                        let r = c.store.move_to(dragged.ix, ix);
                        cx.notify();
                        r
                    });
                    if let Err(e) = result {
                        window.push_notification(
                            format!("Reorder failed: {}", zeble_core::db::error_text(&e)),
                            cx,
                        );
                    }
                }),
            )
            .context_menu(move |menu, _, _| {
                let item = |label: &'static str,
                            action: fn(
                    &mut Launcher,
                    &str,
                    &mut Window,
                    &mut Context<Launcher>,
                )| {
                    let entity = entity.clone();
                    let id = id.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        entity
                            .update(cx, |this, cx| action(this, &id, window, cx))
                            .ok();
                    })
                };
                menu.item(item("Connect", |this, id, _, cx| this.open(id, cx)))
                    .separator()
                    .item(item("Edit", |this, id, window, cx| {
                        this.select(id, window, cx);
                        if let Some(f) = &this.form {
                            f.update(cx, |f, cx| f.focus(window, cx));
                        }
                    }))
                    .item(item("Duplicate", |this, id, window, cx| {
                        this.duplicate(id, window, cx)
                    }))
                    .separator()
                    .item(item("Delete…", |this, id, window, cx| {
                        this.confirm_delete(id, window, cx)
                    }))
            })
    }
}

impl Render for Launcher {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let connections = self.connections.read(cx);
        let list: Vec<ConnectionConfig> = connections.store.connections().to_vec();
        let load_error = connections.load_error.clone();

        h_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                v_flex()
                    .w(px(320.))
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(theme.border)
                    .bg(theme.sidebar)
                    .child(
                        h_flex()
                            .h_12()
                            .px_4()
                            .justify_between()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(Label::new("Connections").font_semibold())
                            .child(
                                Button::new("new-connection")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Plus)
                                    .tooltip("New connection")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.new_connection(window, cx)
                                    })),
                            ),
                    )
                    .when_some(load_error, |d, err| {
                        d.child(
                            div()
                                .m_2()
                                .p_2()
                                .rounded(theme.radius)
                                .bg(theme.danger.opacity(0.1))
                                .text_xs()
                                .text_color(theme.danger)
                                .child(err),
                        )
                    })
                    .child(
                        v_flex()
                            .id("connection-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .p_2()
                            .gap_1()
                            .children(
                                list.iter()
                                    .enumerate()
                                    .map(|(ix, c)| self.render_row(ix, c, cx).into_any_element()),
                            )
                            .when(list.is_empty(), |d| {
                                d.child(
                                    div()
                                        .p_4()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("No saved connections yet."),
                                )
                            }),
                    )
                    .child(
                        div()
                            .px_4()
                            .py_2()
                            .border_t_1()
                            .border_color(theme.border)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Double-click to connect · drag to reorder"),
                    ),
            )
            .child(div().flex_1().h_full().min_w_0().map(|d| {
                match &self.form {
                    Some(form) => d.child(form.clone()),
                    None => d.child(
                        v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .gap_3()
                            .text_color(theme.muted_foreground)
                            .child(Icon::new(IconName::Inbox).size_12())
                            .child("Select a connection to edit it, or double-click to connect.")
                            .child(
                                Button::new("new-connection-2")
                                    .outline()
                                    .label("New Connection")
                                    .icon(IconName::Plus)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.new_connection(window, cx)
                                    })),
                            ),
                    ),
                }
            }))
    }
}
