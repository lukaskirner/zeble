//! The object tree on the left: schemas, grouped by kind, with a filter.
//! Rendered with a virtual list so databases with thousands of objects stay fast.

use std::collections::HashSet;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenuItem},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeble_core::introspect::{DbObject, ObjectKind};

use crate::session::{Session, SessionEvent};

pub enum SidebarEvent {
    Open(DbObject),
    NewQuery,
    Refresh,
}

#[derive(Clone)]
enum Row {
    Schema {
        name: SharedString,
        count: usize,
        expanded: bool,
    },
    Group {
        key: String,
        label: &'static str,
        count: usize,
        expanded: bool,
    },
    Object(DbObject),
}

pub fn kind_icon(kind: ObjectKind) -> Icon {
    Icon::new(match kind {
        ObjectKind::Table => Lucide::Table,
        ObjectKind::View => Lucide::Eye,
        ObjectKind::MaterializedView => Lucide::Layers,
        ObjectKind::ForeignTable => Lucide::Globe,
        ObjectKind::Function => Lucide::SquareFunction,
        ObjectKind::Procedure => Lucide::Workflow,
        ObjectKind::Sequence => Lucide::Hash,
        ObjectKind::Enum => Lucide::ListOrdered,
        ObjectKind::Domain => Lucide::Type,
        ObjectKind::Composite => Lucide::Braces,
        ObjectKind::Range => Lucide::ArrowLeftRight,
    })
}

pub struct Sidebar {
    session: Entity<Session>,
    filter: Entity<InputState>,
    /// Expanded schema (`s:<schema>`) and group (`g:<schema>:<label>`) keys.
    expanded: HashSet<String>,
    defaults_applied: bool,
    rows: Vec<Row>,
    active_key: Option<String>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Sidebar {
    pub fn new(session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter objects"));
        let subscriptions = vec![
            cx.subscribe(&session, |this, _, ev, cx| {
                if let SessionEvent::CatalogChanged = ev {
                    this.rebuild(cx);
                }
            }),
            cx.observe(&session, |_, _, cx| cx.notify()),
            cx.subscribe(&filter, |this, _, ev, cx| {
                if let InputEvent::Change = ev {
                    this.rebuild(cx);
                }
            }),
        ];
        let mut this = Self {
            session,
            filter,
            expanded: HashSet::new(),
            defaults_applied: false,
            rows: Vec::new(),
            active_key: None,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        this.rebuild(cx);
        this
    }

    pub fn set_active(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        if self.active_key != key {
            self.active_key = key;
            cx.notify();
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let Some(catalog) = self.session.read(cx).catalog.clone() else {
            self.rows.clear();
            cx.notify();
            return;
        };
        if !self.defaults_applied {
            // Open `public` (or the only schema) with its tables and views.
            let open: Vec<&String> = if catalog.schemas.len() == 1 {
                catalog.schemas.iter().collect()
            } else {
                catalog.schemas.iter().filter(|s| *s == "public").collect()
            };
            for s in &catalog.schemas {
                self.expanded.insert(format!("g:{s}:Tables"));
                self.expanded.insert(format!("g:{s}:Views"));
            }
            for s in open {
                self.expanded.insert(format!("s:{s}"));
            }
            self.defaults_applied = true;
        }

        let needle = self.filter.read(cx).value().trim().to_lowercase();
        let filtering = !needle.is_empty();
        let matches = |o: &DbObject| !filtering || o.name.to_lowercase().contains(&needle);

        let mut rows = Vec::new();
        for schema in &catalog.schemas {
            let objects: Vec<&DbObject> = catalog
                .objects
                .iter()
                .filter(|o| &o.schema == schema && matches(o))
                .collect();
            if filtering && objects.is_empty() {
                continue;
            }
            let s_expanded = filtering || self.expanded.contains(&format!("s:{schema}"));
            rows.push(Row::Schema {
                name: schema.clone().into(),
                count: objects.len(),
                expanded: s_expanded,
            });
            if !s_expanded {
                continue;
            }
            let mut i = 0;
            while i < objects.len() {
                let label = objects[i].kind.group_label();
                let group: Vec<&DbObject> = objects[i..]
                    .iter()
                    .take_while(|o| o.kind.group_label() == label)
                    .copied()
                    .collect();
                i += group.len();
                let key = format!("g:{schema}:{label}");
                let g_expanded = filtering || self.expanded.contains(&key);
                rows.push(Row::Group {
                    key,
                    label,
                    count: group.len(),
                    expanded: g_expanded,
                });
                if g_expanded {
                    rows.extend(group.into_iter().cloned().map(Row::Object));
                }
            }
        }
        self.rows = rows;
        cx.notify();
    }

    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.filter.read(cx).value().trim().is_empty() {
            return;
        }
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        self.rebuild(cx);
    }

    fn render_row(&self, ix: usize, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let base = h_flex()
            .id(("sidebar-row", ix))
            .h(px(26.))
            .w_full()
            .gap_1p5()
            .pr_2()
            .rounded(theme.radius)
            .text_sm()
            .cursor_pointer()
            .hover(|d| d.bg(theme.sidebar_accent.opacity(0.6)));
        let chevron = |expanded: bool| {
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall()
            .text_color(theme.muted_foreground)
        };
        match row.clone() {
            Row::Schema {
                name,
                count,
                expanded,
            } => base
                .pl_1()
                .child(chevron(expanded))
                .child(
                    Icon::new(Lucide::Database)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(div().flex_1().truncate().font_medium().child(name.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(count.to_string()),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle(format!("s:{name}"), cx)))
                .into_any_element(),
            Row::Group {
                key,
                label,
                count,
                expanded,
            } => base
                .pl_5()
                .child(chevron(expanded))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.muted_foreground)
                        .child(label.to_uppercase()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(count.to_string()),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle(key.clone(), cx)))
                .into_any_element(),
            Row::Object(obj) => {
                let active = self.active_key.as_deref() == Some(obj.key().as_str());
                let open = obj.clone();
                let menu_obj = obj.clone();
                let weak = cx.entity().downgrade();
                base.pl(px(38.))
                    .when(active, |d| {
                        d.bg(theme.sidebar_accent)
                            .text_color(theme.sidebar_accent_foreground)
                    })
                    .child(kind_icon(obj.kind).small().text_color(if active {
                        theme.sidebar_accent_foreground
                    } else {
                        theme.muted_foreground
                    }))
                    .child(div().flex_1().truncate().child(obj.display_name()))
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::Open(open.clone()))),
                    )
                    .context_menu(move |menu, _, _| {
                        let o1 = menu_obj.clone();
                        let o2 = menu_obj.clone();
                        let w = weak.clone();
                        menu.item(PopupMenuItem::new("Open").on_click(move |_, _, cx| {
                            w.update(cx, |_, cx| cx.emit(SidebarEvent::Open(o1.clone())))
                                .ok();
                        }))
                        .item(PopupMenuItem::new("Copy Name").on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(o2.qualified_name()))
                        }))
                    })
                    .into_any_element()
            }
        }
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let loading = session.catalog_loading;
        let error = session.catalog_error.clone();
        let has_catalog = session.catalog.is_some();
        let object_count = session.catalog.as_ref().map_or(0, |c| c.objects.len());
        let connected = session.is_connected();
        let connecting = matches!(session.status, crate::session::ConnStatus::Connecting);

        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .p_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div().flex_1().child(
                            Input::new(&self.filter)
                                .small()
                                .cleanable(true)
                                .prefix(Icon::new(IconName::Search).xsmall()),
                        ),
                    )
                    .child(
                        Button::new("refresh-schema")
                            .ghost()
                            .small()
                            .icon(Lucide::RefreshCw)
                            .tooltip("Refresh schema (⌘R)")
                            .loading(loading)
                            .disabled(!connected)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::Refresh))),
                    )
                    .child(
                        Button::new("new-query")
                            .ghost()
                            .small()
                            .icon(Lucide::SquareTerminal)
                            .tooltip("New query (⌘T)")
                            .disabled(!connected)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::NewQuery))),
                    ),
            )
            .child(div().flex_1().min_h_0().p_1().map(|d| {
                if let Some(err) = error.filter(|_| !has_catalog) {
                    d.child(div().p_2().text_xs().text_color(theme.danger).child(err))
                } else if !has_catalog {
                    let (spin, text) = if connected {
                        (true, "Loading schema…")
                    } else if connecting {
                        (true, "Connecting…")
                    } else {
                        (false, "Not connected")
                    };
                    d.child(
                        h_flex()
                            .p_3()
                            .gap_2()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .when(spin, |d| d.child(Spinner::new().small()))
                            .child(text),
                    )
                } else if self.rows.is_empty() {
                    d.child(
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("No matching objects"),
                    )
                } else {
                    d.child(
                        uniform_list(
                            "sidebar-rows",
                            self.rows.len(),
                            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .map(|ix| {
                                        let row = this.rows[ix].clone();
                                        this.render_row(ix, &row, cx)
                                    })
                                    .collect()
                            }),
                        )
                        .size_full()
                        .track_scroll(&self.scroll),
                    )
                }
            }))
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{object_count} objects")),
            )
    }
}
