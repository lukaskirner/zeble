//! A query tab: SQL editor on top, results below, split by a draggable
//! divider. Each tab owns a dedicated connection so `BEGIN`/`SET` persist.

use std::{ops::Range, sync::Arc};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    highlighter::{Diagnostic, DiagnosticSeverity},
    input::{
        Editor, EditorState, Enter, InputEvent, Position, RangeDecoration,
        RangeDecorationCollection, RangeDecorationStyle,
    },
    resizable::{resizable_panel, v_resizable},
    tab::{Tab, TabBar},
    table::TableState,
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use parking_lot::Mutex;
use zeble_core::{
    QuerySession,
    query::{MAX_RESULT_ROWS, RunOutput},
};

use crate::{
    grid::{GridColumn, GridDelegate, grid_element, new_grid},
    session::Session,
    util::{duration, keys, on_db, plural, thousands},
};

actions!(query, [RunAll]);

const CONTEXT: &str = "QueryTab";

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        keys("mod-shift-enter"),
        RunAll,
        Some(CONTEXT),
    )]);
}

pub enum QueryTabEvent {
    /// Title or SQL changed: the workspace saves its query tabs.
    Changed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultView {
    Results,
    Messages,
}

pub struct QueryTab {
    session: Entity<Session>,
    pub title: String,
    editor: Entity<EditorState>,
    grid: Entity<TableState<GridDelegate>>,
    conn: Option<Arc<QuerySession>>,
    running: bool,
    output: Option<RunOutput>,
    /// Byte offset in the editor where the SQL that ran starts.
    ran_from: usize,
    pub in_transaction: bool,
    /// Tints the statement ⌘↵ would run.
    highlight: RangeDecorationCollection,
    /// What the tint currently shows: (range, editor focused).
    highlighted: Option<(Range<usize>, bool)>,
    result_view: ResultView,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<QueryTabEvent> for QueryTab {}

impl QueryTab {
    pub fn new(
        session: Entity<Session>,
        title: String,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .placeholder("Write SQL here. ⌘↵ runs the statement at the cursor, ⌘⇧↵ runs all.")
                .default_value(sql)
        });
        let grid = new_grid(GridDelegate::new(), window, cx);
        grid.update(cx, |g, _| {
            g.delegate_mut()
                .set_empty_text("Run a query to see results")
        });
        let highlight = editor.update(cx, |e, cx| {
            e.create_range_decorations_collection(vec![], cx)
        });
        let subscriptions = vec![
            cx.subscribe_in(&editor, window, |this, _, ev: &InputEvent, window, cx| {
                match ev {
                    InputEvent::Change => cx.emit(QueryTabEvent::Changed),
                    InputEvent::Focus | InputEvent::Blur => {}
                    _ => return,
                }
                this.update_highlight(window, cx);
            }),
            // Cursor and selection moves only notify; follow them here.
            cx.observe_in(&editor, window, |this, _, window, cx| {
                this.update_highlight(window, cx)
            }),
        ];
        Self {
            session,
            title,
            editor,
            grid,
            conn: None,
            running: false,
            output: None,
            ran_from: 0,
            in_transaction: false,
            result_view: ResultView::Results,
            highlight,
            highlighted: None,
            _subscriptions: subscriptions,
        }
    }

    /// The statement ⌘↵ runs when nothing is selected.
    fn current_statement(&self, cx: &App) -> Option<Range<usize>> {
        let e = self.editor.read(cx);
        zeble_core::split::statement_at(&e.value(), e.cursor())
    }

    fn has_selection(&self, cx: &App) -> bool {
        !self.editor.read(cx).selected_range().is_empty()
    }

    /// Tint the current statement: none while text is selected, dimmer
    /// while the editor is not focused.
    fn update_highlight(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.editor.read(cx).focus_handle(cx).is_focused(window);
        let want = if self.has_selection(cx) {
            None
        } else {
            self.current_statement(cx).map(|r| (r, focused))
        };
        if want == self.highlighted {
            return;
        }
        let color = cx.theme().info.opacity(if focused { 0.14 } else { 0.06 });
        let decorations = want
            .iter()
            .map(|(r, _)| {
                RangeDecoration::new(r.clone())
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(color)
            })
            .collect();
        self.highlight.set(decorations, cx);
        self.highlighted = want;
        cx.notify();
    }

    pub fn sql(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    pub fn focus_editor(&self, window: &mut Window, cx: &mut App) {
        self.editor.update(cx, |e, cx| e.focus(window, cx));
    }

    /// Run the selection if there is one; otherwise everything (`all`) or
    /// the statement at the cursor.
    pub fn run(&mut self, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let (sql, from) = {
            let e = self.editor.read(cx);
            let text = e.value().to_string();
            let range = e.selected_range();
            if !range.is_empty() {
                (e.selected_text().to_string(), range.start)
            } else if all {
                (text, 0)
            } else {
                match zeble_core::split::statement_at(&text, e.cursor()) {
                    Some(r) => (text[r.clone()].to_string(), r.start),
                    None => return,
                }
            }
        };
        if sql.trim().is_empty() {
            return;
        }
        self.clear_diagnostics(window, cx);
        self.ran_from = from;
        self.running = true;

        let Some(db) = self.session.read(cx).db() else {
            self.running = false;
            self.output = Some(RunOutput {
                error: Some(zeble_core::query::QueryError {
                    message: "Not connected".into(),
                    ..Default::default()
                }),
                ..Default::default()
            });
            cx.notify();
            return;
        };
        let existing = self.conn.clone().filter(|c| !c.is_closed());
        let slot = Arc::new(Mutex::new(existing.as_ref().map(|c| c.canceller())));
        let first_line = sql.trim().lines().next().unwrap_or("").to_string();
        let label = format!(
            "Running {}…",
            if first_line.chars().count() > 40 {
                format!("{}…", first_line.chars().take(40).collect::<String>())
            } else {
                first_line
            }
        );
        let activity = self
            .session
            .update(cx, |s, cx| s.begin(label, slot.clone(), cx));
        let work = on_db(async move {
            let conn = match existing {
                Some(c) => c,
                None => {
                    let c = Arc::new(db.open_session().await?);
                    *slot.lock() = Some(c.canceller());
                    c
                }
            };
            let out = conn.run(&sql).await;
            slot.lock().take();
            anyhow::Ok((conn, out))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await.and_then(|r| r);
            this.update_in(cx, |this, window, cx| {
                this.running = false;
                this.session.update(cx, |s, cx| s.end(activity, cx));
                match result {
                    Ok((conn, out)) => {
                        let lost = out.error.as_ref().is_some_and(|e| e.connection_lost);
                        this.conn = if lost { None } else { Some(conn) };
                        this.in_transaction = out.in_transaction && !lost;
                        this.show_output(out, window, cx);
                    }
                    Err(e) => {
                        this.session.update(cx, |s, cx| s.note_error(&e, cx));
                        this.conn = None;
                        this.in_transaction = false;
                        this.show_output(
                            RunOutput {
                                error: Some(zeble_core::query::QueryError {
                                    message: zeble_core::db::error_text(&e),
                                    connection_lost: true,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                            window,
                            cx,
                        );
                    }
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn show_output(&mut self, out: RunOutput, window: &mut Window, cx: &mut Context<Self>) {
        let last = out.last_result().and_then(|s| s.result.clone());
        let has_rows = last.is_some();
        self.grid.update(cx, |g, cx| {
            let d = g.delegate_mut();
            match last {
                Some(rs) => {
                    d.set_columns(
                        rs.columns
                            .iter()
                            .map(|c| GridColumn {
                                name: c.clone().into(),
                                data_type: None,
                                numeric: false,
                            })
                            .collect(),
                    );
                    d.set_rows(Arc::new(rs.rows));
                    d.set_empty_text("No rows");
                }
                None => {
                    d.set_columns(Vec::new());
                    d.set_rows(Arc::new(Vec::new()));
                    d.set_empty_text("The statement returned no rows");
                }
            }
            g.refresh(cx);
            g.scroll_to_row(0, cx);
            g.clear_selection(cx);
        });
        if let Some(err) = &out.error
            && let Some(pos) = err.position
        {
            self.mark_error(pos, &err.message, window, cx);
        }
        self.result_view = if has_rows || out.statements.is_empty() {
            ResultView::Results
        } else {
            ResultView::Messages
        };
        self.output = Some(out);
        cx.notify();
    }

    fn clear_diagnostics(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, cx| {
            if let Some(d) = e.diagnostics_mut() {
                d.clear();
            }
            cx.notify();
        });
    }

    /// Underline the word at `char_pos` (into the SQL that ran).
    fn mark_error(
        &mut self,
        char_pos: usize,
        message: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.sql(cx);
        let start_byte = self.ran_from
            + text[self.ran_from..]
                .char_indices()
                .nth(char_pos)
                .map_or(text.len() - self.ran_from, |(i, _)| i);
        let end_byte = text[start_byte..]
            .char_indices()
            .find(|(_, c)| c.is_whitespace() || matches!(c, ';' | ',' | '(' | ')'))
            .map_or(text.len(), |(i, _)| start_byte + i.max(1));
        let to_pos = |byte: usize| {
            let before = &text[..byte.min(text.len())];
            let line = before.matches('\n').count() as u32;
            let col_start = before.rfind('\n').map_or(0, |i| i + 1);
            let character = before[col_start..].encode_utf16().count() as u32;
            Position { line, character }
        };
        let (start, end) = (to_pos(start_byte), to_pos(end_byte));
        let message = message.to_string();
        self.editor.update(cx, |e, cx| {
            if let Some(d) = e.diagnostics_mut() {
                d.push(
                    Diagnostic::new(start..end, message).with_severity(DiagnosticSeverity::Error),
                );
            }
            cx.notify();
        });
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = &self.conn {
            c.canceller().cancel();
        }
        cx.notify();
    }

    /// Roll back an open transaction and drop the connection (tab closing).
    pub fn close_connection(&mut self) {
        if let Some(conn) = self.conn.take() {
            let in_tx = self.in_transaction;
            zeble_core::runtime().spawn(async move {
                if in_tx {
                    let _ = conn.rollback().await;
                }
                drop(conn);
            });
        }
        self.in_transaction = false;
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut parts: Vec<String> = Vec::new();
        if let Some(out) = &self.output {
            if let Some(st) = out.last_result() {
                let shown = st.result.as_ref().map_or(0, |r| r.rows.len()) as u64;
                parts.push(plural(st.row_count, "row", "rows"));
                if st.truncated {
                    parts.push(format!(
                        "showing first {} (truncated)",
                        thousands(shown.min(MAX_RESULT_ROWS as u64))
                    ));
                }
            } else if let Some(st) = out.statements.last() {
                parts.push(format!("{} affected", plural(st.row_count, "row", "rows")));
            }
            if out.statements.len() > 1 {
                parts.push(format!("{} statements", out.statements.len()));
            }
            parts.push(duration(out.elapsed));
        }
        let truncated = self
            .output
            .as_ref()
            .and_then(|o| o.last_result())
            .is_some_and(|s| s.truncated);
        h_flex()
            .h_9()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .text_sm()
            .child(
                TabBar::new("result-view")
                    .segmented()
                    .xsmall()
                    .selected_index(if self.result_view == ResultView::Results {
                        0
                    } else {
                        1
                    })
                    .child(Tab::new().label("Results"))
                    .child(Tab::new().label("Messages"))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.result_view = if *ix == 0 {
                            ResultView::Results
                        } else {
                            ResultView::Messages
                        };
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_color(if truncated {
                        theme.warning
                    } else {
                        theme.muted_foreground
                    })
                    .child(parts.join(" · ")),
            )
            .child(div().flex_1())
            .when(self.in_transaction, |d| {
                d.child(Tag::warning().small().child("Transaction open"))
            })
    }

    fn render_messages(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(out) = &self.output else {
            return v_flex().into_any_element();
        };
        v_flex()
            .id("messages")
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .gap_1()
            .text_sm()
            .font_family("monospace")
            .children(out.statements.iter().enumerate().map(|(i, st)| {
                let what = if st.result.is_some() {
                    format!("{} returned", plural(st.row_count, "row", "rows"))
                } else {
                    format!("{} affected", plural(st.row_count, "row", "rows"))
                };
                div().child(format!("{}. {} · {}", i + 1, what, duration(st.elapsed)))
            }))
            .when_some(out.error.as_ref(), |d, e| {
                d.child(div().text_color(theme.danger).child(format!(
                    "{}. ERROR: {}",
                    out.statements.len() + 1,
                    e.message
                )))
            })
            .into_any_element()
    }

    fn render_error(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let theme = cx.theme().clone();
        let err = self.output.as_ref()?.error.clone()?;
        Some(
            v_flex()
                .px_3()
                .py_2()
                .gap_1()
                .bg(theme.danger.opacity(0.08))
                .border_b_1()
                .border_color(theme.danger.opacity(0.3))
                .text_sm()
                .child(
                    div()
                        .text_color(theme.danger)
                        .font_medium()
                        .child(match &err.code {
                            Some(code) => format!("ERROR {code}: {}", err.message),
                            None => format!("ERROR: {}", err.message),
                        }),
                )
                .when_some(err.detail.clone(), |d, t| {
                    d.child(div().text_color(theme.muted_foreground).child(t))
                })
                .when_some(err.hint.clone(), |d, t| {
                    d.child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(format!("Hint: {t}")),
                    )
                }),
        )
    }
}

impl Render for QueryTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let connected = self.session.read(cx).is_connected();
        let has_text = !self.editor.read(cx).value().trim().is_empty();
        let can_run = self.has_selection(cx) || self.current_statement(cx).is_some();
        let toolbar = h_flex()
            .h_10()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("run")
                    .small()
                    .primary()
                    .icon(Lucide::Play)
                    .label("Run")
                    .tooltip("Run the statement at the cursor, or the selection (⌘↵)")
                    .loading(self.running)
                    .disabled(self.running || !connected || !can_run)
                    .on_click(cx.listener(|this, _, window, cx| this.run(false, window, cx))),
            )
            .child(
                Button::new("run-all")
                    .small()
                    .outline()
                    .icon(Lucide::ListOrdered)
                    .label("Run All")
                    .tooltip("Run everything in the editor (⌘⇧↵)")
                    .disabled(self.running || !connected || !has_text)
                    .on_click(cx.listener(|this, _, window, cx| this.run(true, window, cx))),
            )
            .when(self.running, |d| {
                d.child(
                    Button::new("cancel-query")
                        .small()
                        .outline()
                        .icon(Lucide::SquareStop)
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            .child(div().flex_1())
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                if self.conn.is_some() {
                    "Dedicated session"
                } else {
                    "Session opens on first run"
                },
            ));

        let error = self.render_error(cx);
        let results = v_flex()
            .size_full()
            .child(self.render_status(cx))
            .children(error)
            .child(div().flex_1().min_h_0().map(|d| match self.result_view {
                ResultView::Results => d.child(grid_element(&self.grid, cx)),
                ResultView::Messages => d.child(self.render_messages(cx)),
            }));

        v_flex()
            .size_full()
            .key_context(CONTEXT)
            // Cmd/Ctrl+Enter would insert a newline in the editor; take it first.
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if action.secondary {
                    cx.stop_propagation();
                    this.run(false, window, cx);
                }
            }))
            .capture_action(cx.listener(|this, _: &RunAll, window, cx| {
                cx.stop_propagation();
                this.run(true, window, cx);
            }))
            .child(toolbar)
            .child(
                div().flex_1().min_h_0().child(
                    v_resizable("query-split")
                        .child(
                            resizable_panel()
                                .size(px(220.))
                                .size_range(px(80.)..Pixels::MAX)
                                .child(Editor::new(&self.editor).bordered(false).h_full()),
                        )
                        .child(resizable_panel().child(results)),
                ),
            )
    }
}
