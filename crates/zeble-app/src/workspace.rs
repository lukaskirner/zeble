//! The connection window: top bar, object sidebar and tabs.

use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    resizable::{h_resizable, resizable_panel},
    spinner::Spinner,
    tab::{Tab, TabBar},
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeble_core::{
    ConnectionConfig,
    introspect::DbObject,
    store::{QueryTabsFile, SavedQuery},
};

use crate::{
    object_tab::ObjectTab,
    query_tab::{QueryTab, QueryTabEvent},
    relation_tab::RelationTab,
    session::{ConnStatus, Session},
    sidebar::{Sidebar, SidebarEvent, kind_icon},
    util::{color_tag, duration, keys},
};

actions!(
    workspace,
    [NewQueryTab, CloseTab, RefreshSchema, NextTab, PrevTab]
);

const CONTEXT: &str = "ConnectionWindow";

pub fn init(cx: &mut App) {
    crate::grid::init(cx);
    cx.bind_keys([
        KeyBinding::new(keys("mod-t"), NewQueryTab, Some(CONTEXT)),
        KeyBinding::new(keys("mod-w"), CloseTab, Some(CONTEXT)),
        KeyBinding::new(keys("mod-r"), RefreshSchema, Some(CONTEXT)),
        KeyBinding::new("ctrl-tab", NextTab, Some(CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some(CONTEXT)),
    ]);
}

pub fn open_connection_window(config: ConnectionConfig, cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
        window_min_size: Some(size(px(720.), px(480.))),
        titlebar: Some(TitlebarOptions {
            title: Some(format!("{} — Zeble", config.display_name()).into()),
            ..Default::default()
        }),
        app_id: Some("zeble".into()),
        ..Default::default()
    };
    let result = gpui_kit::open_window(options, cx, |window, cx| {
        let view = cx.new(|cx| ConnectionWindow::new(config, window, cx));
        let weak = view.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.should_close(window, cx))
                .unwrap_or(true)
        });
        view
    });
    if let Err(e) = result {
        eprintln!("failed to open connection window: {e:#}");
    }
}

#[derive(Clone)]
enum TabContent {
    Relation(Entity<RelationTab>),
    Object(Entity<ObjectTab>),
    Query(Entity<QueryTab>),
}

#[derive(Clone)]
struct OpenTab {
    id: u64,
    content: TabContent,
}

impl OpenTab {
    fn title(&self, cx: &App) -> String {
        match &self.content {
            TabContent::Relation(t) => t.read(cx).title(),
            TabContent::Object(t) => t.read(cx).title(),
            TabContent::Query(t) => t.read(cx).title.clone(),
        }
    }

    fn object_key(&self, cx: &App) -> Option<String> {
        match &self.content {
            TabContent::Relation(t) => Some(t.read(cx).object.key()),
            TabContent::Object(t) => Some(t.read(cx).object.key()),
            TabContent::Query(_) => None,
        }
    }

    fn icon(&self, cx: &App) -> Icon {
        match &self.content {
            TabContent::Relation(t) => kind_icon(t.read(cx).object.kind),
            TabContent::Object(t) => kind_icon(t.read(cx).object.kind),
            TabContent::Query(_) => Icon::new(Lucide::SquareTerminal),
        }
    }

    fn view(&self) -> AnyView {
        match &self.content {
            TabContent::Relation(t) => t.clone().into(),
            TabContent::Object(t) => t.clone().into(),
            TabContent::Query(t) => t.clone().into(),
        }
    }
}

#[derive(Clone)]
struct DraggedTab {
    ix: usize,
    title: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .text_sm()
            .child(self.title.clone())
    }
}

pub struct ConnectionWindow {
    session: Entity<Session>,
    sidebar: Entity<Sidebar>,
    tabs: Vec<OpenTab>,
    active: Option<usize>,
    next_tab_id: u64,
    query_counter: usize,
    save_task: Option<Task<()>>,
    /// Set once the user confirmed closing despite open transactions.
    close_confirmed: bool,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl ConnectionWindow {
    fn new(config: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let conn_id = config.id.clone();
        let session = cx.new(|cx| Session::new(config, cx));
        let sidebar = cx.new(|cx| Sidebar::new(session.clone(), window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&sidebar, window, |this, _, ev, window, cx| match ev {
                SidebarEvent::Open(obj) => this.open_object(obj.clone(), window, cx),
                SidebarEvent::NewQuery => this.new_query(window, cx),
                SidebarEvent::Refresh => this.refresh_schema(cx),
            }),
            cx.observe(&session, |_, _, cx| cx.notify()),
        ];
        window
            .observe_window_appearance(|window, cx| {
                gpui_kit::component::Theme::sync_system_appearance(Some(window), cx)
            })
            .detach();
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let mut this = Self {
            session,
            sidebar,
            tabs: Vec::new(),
            active: None,
            next_tab_id: 0,
            query_counter: 0,
            save_task: None,
            close_confirmed: false,
            focus_handle,
            _subscriptions: subscriptions,
        };
        for saved in QueryTabsFile::load(&conn_id) {
            this.add_query_tab(saved.title, saved.sql, window, cx);
        }
        this
    }

    fn push_tab(&mut self, content: TabContent, cx: &mut Context<Self>) {
        self.next_tab_id += 1;
        self.tabs.push(OpenTab {
            id: self.next_tab_id,
            content,
        });
        self.activate(self.tabs.len() - 1, cx);
    }

    fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.active =
            (!self.tabs.is_empty()).then(|| ix.min(self.tabs.len() - 1));
        let key = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|t| t.object_key(cx));
        self.sidebar.update(cx, |s, cx| s.set_active(key, cx));
        cx.notify();
    }

    fn open_object(&mut self, obj: DbObject, window: &mut Window, cx: &mut Context<Self>) {
        let key = obj.key();
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|t| t.object_key(cx).as_deref() == Some(key.as_str()))
        {
            self.activate(ix, cx);
            return;
        }
        let session = self.session.clone();
        let content = if obj.kind.is_relation() {
            TabContent::Relation(cx.new(|cx| RelationTab::new(session, obj, window, cx)))
        } else {
            TabContent::Object(cx.new(|cx| ObjectTab::new(session, obj, window, cx)))
        };
        self.push_tab(content, cx);
    }

    fn add_query_tab(
        &mut self,
        title: String,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<QueryTab> {
        self.query_counter += 1;
        let session = self.session.clone();
        let tab = cx.new(|cx| QueryTab::new(session, title, sql, window, cx));
        self._subscriptions
            .push(cx.subscribe(&tab, |this, _, ev, cx| match ev {
                QueryTabEvent::Changed => this.schedule_save(cx),
            }));
        self.push_tab(TabContent::Query(tab.clone()), cx);
        tab
    }

    fn new_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Number after the highest "Query N" still open.
        let n = self
            .tabs
            .iter()
            .filter_map(|t| match &t.content {
                TabContent::Query(q) => q
                    .read(cx)
                    .title
                    .strip_prefix("Query ")
                    .and_then(|n| n.parse::<usize>().ok()),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            + 1;
        let tab = self.add_query_tab(format!("Query {n}"), String::new(), window, cx);
        tab.update(cx, |t, cx| t.focus_editor(window, cx));
        self.schedule_save(cx);
    }

    fn refresh_schema(&mut self, cx: &mut Context<Self>) {
        self.session.update(cx, |s, cx| {
            if s.is_connected() {
                s.load_catalog(cx)
            }
        });
    }

    fn saved_queries(&self, cx: &App) -> Vec<SavedQuery> {
        self.tabs
            .iter()
            .filter_map(|t| match &t.content {
                TabContent::Query(q) => {
                    let q = q.read(cx);
                    Some(SavedQuery {
                        title: q.title.clone(),
                        sql: q.sql(cx),
                    })
                }
                _ => None,
            })
            .collect()
    }

    fn save_now(&mut self, cx: &mut Context<Self>) {
        self.save_task = None;
        let id = self.session.read(cx).config.id.clone();
        if let Err(e) = QueryTabsFile::save(&id, self.saved_queries(cx)) {
            eprintln!("saving query tabs failed: {e:#}");
        }
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            this.update(cx, |this, cx| this.save_now(cx)).ok();
        }));
    }

    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix).cloned() else {
            return;
        };
        if let TabContent::Query(q) = &tab.content
            && q.read(cx).in_transaction
        {
            let weak = cx.entity().downgrade();
            let tab_id = tab.id;
            let title = q.read(cx).title.clone();
            window.open_alert_dialog(cx, move |alert, _, _| {
                let weak = weak.clone();
                alert
                    .title(format!("\"{title}\" has an open transaction"))
                    .description("Closing the tab rolls the transaction back.")
                    .confirm()
                    .ok_text("Roll Back and Close")
                    .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                    .on_ok(move |_, _, cx| {
                        weak.update(cx, |this, cx| {
                            if let Some(ix) = this.tabs.iter().position(|t| t.id == tab_id) {
                                this.remove_tab(ix, cx);
                            }
                        })
                        .ok();
                        true
                    })
            });
            return;
        }
        self.remove_tab(ix, cx);
    }

    fn remove_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        let tab = self.tabs.remove(ix);
        if let TabContent::Query(q) = &tab.content {
            q.update(cx, |q, _| q.close_connection());
            self.save_now(cx);
        }
        let new_active = match self.active {
            Some(a) if a > ix => Some(a - 1),
            Some(a) if a == ix => Some(ix.min(self.tabs.len().saturating_sub(1))),
            other => other,
        };
        self.activate(new_active.unwrap_or(0), cx);
    }

    fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from == to || from >= self.tabs.len() {
            return;
        }
        let active_id = self.active.and_then(|a| self.tabs.get(a)).map(|t| t.id);
        let tab = self.tabs.remove(from);
        self.tabs.insert(to.min(self.tabs.len()), tab);
        if let Some(id) = active_id {
            self.active = self.tabs.iter().position(|t| t.id == id);
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Window close: confirm when query tabs have open transactions.
    fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let open_tx = self
            .tabs
            .iter()
            .filter(|t| matches!(&t.content, TabContent::Query(q) if q.read(cx).in_transaction))
            .count();
        if open_tx > 0 && !self.close_confirmed {
            let weak = cx.entity().downgrade();
            window.open_alert_dialog(cx, move |alert, _, _| {
                let weak = weak.clone();
                alert
                    .title("Close with open transactions?")
                    .description(format!(
                        "{open_tx} query tab{} {} an open transaction. Closing the window rolls {} back.",
                        if open_tx == 1 { "" } else { "s" },
                        if open_tx == 1 { "has" } else { "have" },
                        if open_tx == 1 { "it" } else { "them" },
                    ))
                    .confirm()
                    .ok_text("Roll Back and Close")
                    .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                    .on_ok(move |_, window, cx| {
                        weak.update(cx, |this, _| this.close_confirmed = true).ok();
                        window.remove_window();
                        true
                    })
            });
            return false;
        }
        self.save_now(cx);
        for t in &self.tabs {
            if let TabContent::Query(q) = &t.content {
                q.update(cx, |q, _| q.close_connection());
            }
        }
        true
    }

    fn render_top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let config = &session.config;
        let (dot, status_text): (Hsla, SharedString) = match &session.status {
            ConnStatus::Connecting => (theme.warning, "Connecting…".into()),
            ConnStatus::Connected { version } => (
                theme.success,
                format!("Connected · PostgreSQL {version}").into(),
            ),
            ConnStatus::Disconnected(msg) => (
                theme.muted_foreground,
                format!("Disconnected · {msg}").into(),
            ),
            ConnStatus::Error(msg) => (theme.danger, format!("Connection failed · {msg}").into()),
        };
        let can_reconnect = matches!(
            session.status,
            ConnStatus::Disconnected(_) | ConnStatus::Error(_)
        );
        let activity = session.activities.first().map(|a| {
            let more = session.activities.len() - 1;
            format!(
                "{}  {}{}",
                a.label,
                duration(a.started.elapsed()),
                if more > 0 {
                    format!("  (+{more} more)")
                } else {
                    String::new()
                }
            )
        });
        let can_cancel = session.can_cancel();

        h_flex()
            .h(px(44.))
            .flex_none()
            .px_3()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .when_some(config.color, |d, c| {
                d.border_t_4().border_color(color_tag(c))
            })
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .flex_shrink(1.)
                    .when_some(config.color, |d, c| {
                        d.child(div().size_2p5().rounded_full().bg(color_tag(c)))
                    })
                    .child(
                        div()
                            .font_semibold()
                            .text_sm()
                            .truncate()
                            .child(config.display_name()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(config.summary()),
                    )
                    .when(config.read_only, |d| {
                        d.child(Tag::secondary().small().child("Read-only"))
                    }),
            )
            .child(
                h_flex()
                    .id("conn-status")
                    .gap_1p5()
                    .min_w_0()
                    .flex_shrink(1.)
                    .text_xs()
                    .child(div().flex_none().size_2().rounded_full().bg(dot))
                    .child(div().truncate().child(status_text.clone()))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(status_text.clone())
                            .build(window, cx)
                    }),
            )
            .when(can_reconnect, |d| {
                d.child(
                    Button::new("reconnect")
                        .xsmall()
                        .outline()
                        .icon(Lucide::RefreshCw)
                        .label("Reconnect")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.session.update(cx, |s, cx| s.connect(cx))
                        })),
                )
            })
            .child(div().flex_1())
            .map(|d| match activity {
                Some(text) => d
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(Spinner::new().xsmall())
                            .child(div().truncate().child(text)),
                    )
                    .when(can_cancel, |d| {
                        d.child(
                            Button::new("cancel-all")
                                .xsmall()
                                .outline()
                                .icon(Lucide::SquareStop)
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.session.update(cx, |s, _| s.cancel_all())
                                })),
                        )
                    }),
                None => d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Idle"),
                ),
            })
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let title: SharedString = tab.title(cx).into();
                Tab::new()
                    .label(title.clone())
                    .prefix(div().pl_2().child(tab.icon(cx).small()))
                    .suffix(
                        Button::new(("close-tab", tab.id))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(ix, window, cx)
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.activate(ix, cx)))
                    .on_drag(DraggedTab { ix, title }, |drag, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| drag.clone())
                    })
                    .drag_over::<DraggedTab>(|d, _, _, cx| {
                        d.border_l_2().border_color(cx.theme().drag_border)
                    })
                    .on_drop(cx.listener(move |this, drag: &DraggedTab, _, cx| {
                        this.move_tab(drag.ix, ix, cx)
                    }))
            })
            .collect::<Vec<_>>();
        TabBar::new("tabs")
            .when_some(self.active, |t, a| t.selected_index(a))
            .children(tabs)
            .suffix(
                Button::new("new-query-tab")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .tooltip("New query (⌘T)")
                    .on_click(cx.listener(|this, _, window, cx| this.new_query(window, cx))),
            )
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .text_color(theme.muted_foreground)
            .child(Icon::new(Lucide::Table).size_12())
            .child("Select a table in the sidebar, or open a query tab.")
            .child(
                Button::new("empty-new-query")
                    .outline()
                    .icon(Lucide::SquareTerminal)
                    .label("New Query  ⌘T")
                    .on_click(cx.listener(|this, _, window, cx| this.new_query(window, cx))),
            )
    }
}

impl Focusable for ConnectionWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ConnectionWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let active_view = self.active.and_then(|a| self.tabs.get(a)).map(|t| t.view());
        v_flex()
            .size_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(|this, _: &NewQueryTab, window, cx| this.new_query(window, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if let Some(a) = this.active {
                    this.close_tab(a, window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &RefreshSchema, _, cx| this.refresh_schema(cx)))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| {
                if !this.tabs.is_empty() {
                    let next = this.active.map_or(0, |a| (a + 1) % this.tabs.len());
                    this.activate(next, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &PrevTab, _, cx| {
                if !this.tabs.is_empty() {
                    let n = this.tabs.len();
                    let prev = this.active.map_or(0, |a| (a + n - 1) % n);
                    this.activate(prev, cx)
                }
            }))
            .child(self.render_top_bar(cx))
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("workspace-split")
                        .child(
                            resizable_panel()
                                .size(px(270.))
                                .size_range(px(180.)..px(560.))
                                .child(self.sidebar.clone()),
                        )
                        .child(
                            resizable_panel().child(
                                v_flex()
                                    .size_full()
                                    .when(!self.tabs.is_empty(), |d| {
                                        d.child(self.render_tab_bar(cx))
                                    })
                                    .child(div().flex_1().min_h_0().map(|d| match active_view {
                                        Some(v) => d.child(v),
                                        None => d.child(self.render_empty(cx)),
                                    })),
                            ),
                        ),
                ),
            )
    }
}
