//! A table or view tab: paged Data with header sorting, and Structure.

use std::{sync::Arc, time::Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, IndexPath, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState},
    label::Label,
    select::{Select, SelectEvent, SelectState},
    tab::{Tab, TabBar},
    table::TableState,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeble_core::{
    introspect::{DbObject, RelationInfo, load_relation},
    query::{count_rows, fetch_page},
    sql::{PageQuery, SortDir, StableOrder},
};

use crate::{
    grid::{GridColumn, GridDelegate, grid_element, new_grid},
    session::{Session, SessionEvent},
    util::{duration, thousands},
};

pub const PAGE_SIZES: [usize; 4] = [100, 500, 1000, 5000];
pub const DEFAULT_PAGE_SIZE: usize = 500;

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Data,
    Structure,
}

pub struct RelationTab {
    session: Entity<Session>,
    pub object: DbObject,
    view: View,
    info: Option<Arc<RelationInfo>>,
    info_error: Option<SharedString>,
    grid: Entity<TableState<GridDelegate>>,
    page: usize,
    page_size: usize,
    sort: Option<(usize, bool)>,
    rows_on_page: usize,
    exact_count: Option<u64>,
    counting: bool,
    loading: bool,
    load_error: Option<SharedString>,
    last_elapsed: Option<std::time::Duration>,
    page_size_select: Entity<SelectState<Vec<SharedString>>>,
    /// Increments per request so a slow, stale response is dropped.
    load_seq: u64,
    definition: Option<Entity<EditorState>>,
    _subscriptions: Vec<Subscription>,
}

impl RelationTab {
    pub fn new(
        session: Entity<Session>,
        object: DbObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let weak = cx.entity().downgrade();
        let grid = new_grid(
            GridDelegate::new().on_sort(move |col, _, cx| {
                weak.update(cx, |this, cx| this.cycle_sort(col, cx)).ok();
            }),
            window,
            cx,
        );
        let page_size_select = cx.new(|cx| {
            SelectState::new(
                PAGE_SIZES
                    .iter()
                    .map(|s| SharedString::from(s.to_string()))
                    .collect::<Vec<_>>(),
                Some(IndexPath::default().row(1)),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe(
                &page_size_select,
                |this, _, ev: &SelectEvent<Vec<SharedString>>, cx| {
                    if let SelectEvent::Confirm(Some(v)) = ev
                        && let Ok(size) = v.parse::<usize>()
                    {
                        this.page_size = size;
                        this.page = 0;
                        this.load_page(cx);
                    }
                },
            ),
            cx.subscribe(&session, |this, _, ev, cx| {
                if let SessionEvent::Connected = ev {
                    this.reload(cx);
                }
            }),
        ];
        let mut this = Self {
            session,
            object,
            view: View::Data,
            info: None,
            info_error: None,
            grid,
            page: 0,
            page_size: DEFAULT_PAGE_SIZE,
            sort: None,
            rows_on_page: 0,
            exact_count: None,
            counting: false,
            loading: false,
            load_error: None,
            last_elapsed: None,
            page_size_select,
            load_seq: 0,
            definition: None,
            _subscriptions: subscriptions,
        };
        this.reload(cx);
        this
    }

    pub fn title(&self) -> String {
        self.object.name.clone()
    }

    /// Reload structure, then the current page.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let oid = self.object.oid;
        let label = format!("Loading {}…", self.object.qualified_name());
        self.loading = true;
        self.grid.update(cx, |g, cx| {
            g.delegate_mut().loading = true;
            cx.notify();
        });
        let task = self.session.update(cx, |s, cx| {
            s.with_client(label, cx, move |client| async move {
                load_relation(&**client, oid).await
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(info) => {
                        this.info_error = None;
                        let columns = info
                            .columns
                            .iter()
                            .map(|c| GridColumn {
                                name: c.name.clone().into(),
                                data_type: Some(c.data_type.clone().into()),
                                numeric: c.is_numeric(),
                            })
                            .collect();
                        this.definition = None;
                        this.info = Some(Arc::new(info));
                        this.grid
                            .update(cx, |g, _| g.delegate_mut().set_columns(columns));
                        this.load_page(cx);
                    }
                    Err(e) => {
                        this.loading = false;
                        this.info_error = Some(zeble_core::db::error_text(&e).into());
                        this.grid.update(cx, |g, cx| {
                            g.delegate_mut().loading = false;
                            cx.notify();
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn page_query(&self) -> Option<PageQuery> {
        let info = self.info.as_ref()?;
        let stable = if !info.primary_key.is_empty() {
            StableOrder::PrimaryKey(info.primary_key.clone())
        } else if info.has_ctid {
            StableOrder::Ctid
        } else {
            StableOrder::None
        };
        Some(PageQuery {
            schema: self.object.schema.clone(),
            name: self.object.name.clone(),
            sort: self.sort.and_then(|(ix, asc)| {
                info.columns.get(ix).map(|c| {
                    (
                        c.name.clone(),
                        if asc { SortDir::Asc } else { SortDir::Desc },
                    )
                })
            }),
            stable,
            page: self.page,
            page_size: self.page_size,
        })
    }

    fn load_page(&mut self, cx: &mut Context<Self>) {
        let Some(query) = self.page_query() else {
            return;
        };
        self.load_seq += 1;
        let seq = self.load_seq;
        self.loading = true;
        self.load_error = None;
        let label = format!(
            "Loading {} (page {})…",
            self.object.qualified_name(),
            self.page + 1
        );
        self.grid.update(cx, |g, cx| {
            g.delegate_mut().loading = true;
            cx.notify();
        });
        let started = Instant::now();
        let task = self.session.update(cx, |s, cx| {
            s.with_client(label, cx, move |client| async move {
                fetch_page(&client, &query).await
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if seq != this.load_seq {
                    return;
                }
                this.loading = false;
                this.last_elapsed = Some(started.elapsed());
                let sort = this.sort;
                match result {
                    Ok(rs) => {
                        this.rows_on_page = rs.rows.len();
                        this.grid.update(cx, |g, cx| {
                            let d = g.delegate_mut();
                            d.loading = false;
                            d.set_sort(sort);
                            d.set_rows(Arc::new(rs.rows));
                            g.refresh(cx);
                            g.scroll_to_row(0, cx);
                            g.clear_selection(cx);
                        });
                    }
                    Err(e) => {
                        this.load_error = Some(zeble_core::db::error_text(&e).into());
                        this.grid.update(cx, |g, cx| {
                            g.delegate_mut().loading = false;
                            cx.notify();
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Header click: off → ascending → descending → off.
    fn cycle_sort(&mut self, col: usize, cx: &mut Context<Self>) {
        self.sort = match self.sort {
            Some((c, true)) if c == col => Some((col, false)),
            Some((c, false)) if c == col => None,
            _ => Some((col, true)),
        };
        self.page = 0;
        self.load_page(cx);
    }

    fn go_to_page(&mut self, page: usize, cx: &mut Context<Self>) {
        self.page = page;
        self.load_page(cx);
    }

    fn count(&mut self, cx: &mut Context<Self>) {
        let (schema, name) = (self.object.schema.clone(), self.object.name.clone());
        self.counting = true;
        let task = self.session.update(cx, |s, cx| {
            s.with_client(
                format!("Counting rows of {name}…"),
                cx,
                move |client| async move { count_rows(&client, &schema, &name).await },
            )
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.counting = false;
                match result {
                    Ok(n) => this.exact_count = Some(n),
                    Err(e) => this.load_error = Some(zeble_core::db::error_text(&e).into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn total(&self) -> Option<(u64, bool)> {
        if let Some(n) = self.exact_count {
            return Some((n, true));
        }
        self.info
            .as_ref()
            .and_then(|i| i.estimated_rows)
            .map(|n| (n, false))
    }

    fn render_pager(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let total = self.total();
        let first = self.page * self.page_size;
        let rows_text = if self.rows_on_page == 0 {
            "No rows".to_string()
        } else {
            format!(
                "Rows {}–{}",
                thousands(first as u64 + 1),
                thousands((first + self.rows_on_page) as u64)
            )
        };
        let total_text = match total {
            Some((n, true)) => format!(" of {}", thousands(n)),
            Some((n, false)) => format!(" of ~{}", thousands(n)),
            None => String::new(),
        };
        let pages = total.map(|(n, _)| ((n as usize).max(1)).div_ceil(self.page_size));
        let page_text = match (pages, total) {
            (Some(p), Some((_, exact))) => format!(
                "Page {} of {}{}",
                self.page + 1,
                if exact { "" } else { "~" },
                thousands(p.max(self.page + 1) as u64)
            ),
            _ => format!("Page {}", self.page + 1),
        };
        let has_next = self.rows_on_page == self.page_size;
        h_flex()
            .h_10()
            .px_3()
            .gap_3()
            .border_t_1()
            .border_color(theme.border)
            .text_sm()
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(format!("{rows_text}{total_text}")),
            )
            .when(self.exact_count.is_none() && self.info.is_some(), |d| {
                d.child(
                    Button::new("count")
                        .xsmall()
                        .outline()
                        .label("Count")
                        .tooltip("Run SELECT count(*) for the exact number of rows")
                        .loading(self.counting)
                        .disabled(self.counting)
                        .on_click(cx.listener(|this, _, _, cx| this.count(cx))),
                )
            })
            .when_some(self.last_elapsed, |d, e| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(duration(e)),
                )
            })
            .child(div().flex_1())
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Page size"),
                    )
                    .child(
                        div()
                            .w_24()
                            .child(Select::new(&self.page_size_select).xsmall()),
                    ),
            )
            .child(
                Button::new("prev-page")
                    .xsmall()
                    .ghost()
                    .icon(IconName::ChevronLeft)
                    .disabled(self.page == 0 || self.loading)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let p = this.page.saturating_sub(1);
                        this.go_to_page(p, cx)
                    })),
            )
            .child(div().min_w_32().text_center().child(page_text))
            .child(
                Button::new("next-page")
                    .xsmall()
                    .ghost()
                    .icon(IconName::ChevronRight)
                    .disabled(!has_next || self.loading)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let p = this.page + 1;
                        this.go_to_page(p, cx)
                    })),
            )
    }

    fn definition_editor(
        &mut self,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<EditorState> {
        if let Some(e) = &self.definition {
            return e.clone();
        }
        let sql = sql.to_string();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .default_value(sql)
        });
        self.definition = Some(editor.clone());
        editor
    }

    fn render_structure(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(info) = self.info.clone() else {
            return div().into_any_element();
        };
        let columns = simple_table(
            &[
                ("", 0.4),
                ("Column", 2.),
                ("Type", 2.),
                ("Nullable", 0.8),
                ("Default", 2.5),
                ("Comment", 2.),
            ],
            info.columns
                .iter()
                .map(|c| {
                    vec![
                        if c.primary_key {
                            "PK".to_string()
                        } else {
                            String::new()
                        },
                        c.name.clone(),
                        c.data_type.clone(),
                        if c.nullable {
                            "YES".into()
                        } else {
                            "NO".into()
                        },
                        c.default.clone().unwrap_or_default(),
                        c.comment.clone().unwrap_or_default(),
                    ]
                })
                .collect(),
            cx,
        );
        let indexes = simple_table(
            &[("Name", 2.), ("Definition", 6.)],
            info.indexes
                .iter()
                .map(|i| vec![i.name.clone(), i.definition.clone()])
                .collect(),
            cx,
        );
        let constraints = simple_table(
            &[("Name", 2.), ("Type", 1.2), ("Definition", 5.)],
            info.constraints
                .iter()
                .map(|c| vec![c.name.clone(), c.kind.clone(), c.definition.clone()])
                .collect(),
            cx,
        );
        let definition = info
            .view_definition
            .as_deref()
            .map(|sql| self.definition_editor(sql, window, cx));

        v_flex()
            .id("structure")
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .gap_5()
            .when_some(info.comment.clone(), |d, c| {
                d.child(div().text_sm().text_color(theme.muted_foreground).child(c))
            })
            .child(section(
                format!("Columns ({})", info.columns.len()),
                columns,
            ))
            .when(!info.indexes.is_empty(), |d| {
                d.child(section(
                    format!("Indexes ({})", info.indexes.len()),
                    indexes,
                ))
            })
            .when(!info.constraints.is_empty(), |d| {
                d.child(section(
                    format!("Constraints ({})", info.constraints.len()),
                    constraints,
                ))
            })
            .when_some(definition, |d, editor| {
                d.child(section(
                    "Definition",
                    div()
                        .h(px(260.))
                        .child(Editor::new(&editor).readonly(true).h_full()),
                ))
            })
            .into_any_element()
    }
}

pub fn section(title: impl Into<SharedString>, body: impl IntoElement) -> Div {
    v_flex()
        .gap_2()
        .child(Label::new(title.into()).font_semibold())
        .child(body)
}

/// A plain, non-virtual table for the small Structure lists.
pub fn simple_table<T>(
    headers: &[(&'static str, f32)],
    rows: Vec<Vec<String>>,
    cx: &Context<T>,
) -> impl IntoElement + use<T> {
    let theme = cx.theme().clone();
    let weights: Vec<f32> = headers.iter().map(|(_, w)| *w).collect();
    let cell = |text: String, w: f32| {
        div()
            .flex_grow(1.)
            .flex_shrink(1.)
            .flex_basis(relative(w / 10.))
            .min_w_0()
            .px_2()
            .py_1()
            .overflow_hidden()
            .child(text)
    };
    v_flex()
        .w_full()
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .text_sm()
        .child(
            h_flex()
                .bg(theme.table_head)
                .font_medium()
                .text_color(theme.muted_foreground)
                .border_b_1()
                .border_color(theme.border)
                .children(headers.iter().map(|(h, w)| cell(h.to_string(), *w))),
        )
        .children(rows.into_iter().enumerate().map(move |(i, r)| {
            h_flex()
                .when(i > 0, |d| {
                    d.border_t_1().border_color(theme.border.opacity(0.5))
                })
                .children(r.into_iter().zip(weights.iter()).map(|(t, w)| cell(t, *w)))
        }))
}

impl Render for RelationTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let view = self.view;
        let toolbar = h_flex()
            .h_10()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                TabBar::new("relation-view")
                    .segmented()
                    .xsmall()
                    .selected_index(if view == View::Data { 0 } else { 1 })
                    .child(Tab::new().label("Data"))
                    .child(Tab::new().label("Structure"))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.view = if *ix == 0 {
                            View::Data
                        } else {
                            View::Structure
                        };
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(format!(
                        "{} · {}",
                        self.object.qualified_name(),
                        self.object.kind.label()
                    )),
            )
            .child(div().flex_1())
            .child(
                Button::new("reload")
                    .xsmall()
                    .ghost()
                    .icon(Lucide::RefreshCw)
                    .tooltip("Reload")
                    .loading(self.loading)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.exact_count = None;
                        this.reload(cx)
                    })),
            );

        let error = self.info_error.clone().or(self.load_error.clone());
        v_flex()
            .size_full()
            .child(toolbar)
            .when_some(error, |d, err| {
                d.child(
                    div()
                        .px_3()
                        .py_2()
                        .bg(theme.danger.opacity(0.08))
                        .text_sm()
                        .text_color(theme.danger)
                        .child(err),
                )
            })
            .child(div().flex_1().min_h_0().map(|d| match view {
                View::Data => d.child(grid_element(&self.grid, cx)),
                View::Structure => d.child(self.render_structure(window, cx)),
            }))
            .when(view == View::Data, |d| d.child(self.render_pager(cx)))
    }
}
