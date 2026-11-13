//! The result grid: a virtualized `DataTable` over text rows, with sticky
//! headers, NULL rendering and copy support. Used by table and query tabs.

use std::{rc::Rc, sync::Arc};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, StyledExt as _, h_flex,
    table::{Column, ColumnSort, TableDelegate, TableSelection, TableState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::util::cell_preview;

actions!(grid, [CopySelection]);

pub const CONTEXT: &str = "ResultGrid";

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        crate::util::keys("mod-c"),
        CopySelection,
        Some(CONTEXT),
    )]);
}

/// Longest text drawn in one cell.
const CELL_CHARS: usize = 200;

#[derive(Clone)]
pub struct GridColumn {
    pub name: SharedString,
    /// Shown under the name in the header, when known.
    pub data_type: Option<SharedString>,
    pub numeric: bool,
}

pub type Rows = Arc<Vec<Vec<Option<String>>>>;
type SortCallback = Rc<dyn Fn(usize, &mut Window, &mut App)>;

pub struct GridDelegate {
    columns: Vec<GridColumn>,
    widths: Vec<Pixels>,
    rows: Rows,
    pub loading: bool,
    /// Current sort, shown in the header: (column, ascending).
    sort: Option<(usize, bool)>,
    on_sort: Option<SortCallback>,
    empty_text: SharedString,
}

impl GridDelegate {
    pub fn new() -> Self {
        Self {
            columns: Vec::new(),
            widths: Vec::new(),
            rows: Arc::new(Vec::new()),
            loading: false,
            sort: None,
            on_sort: None,
            empty_text: "No rows".into(),
        }
    }

    /// Make headers clickable; `f` gets the clicked column index.
    pub fn on_sort(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_sort = Some(Rc::new(f));
        self
    }

    pub fn set_empty_text(&mut self, text: impl Into<SharedString>) {
        self.empty_text = text.into();
    }

    pub fn set_sort(&mut self, sort: Option<(usize, bool)>) {
        self.sort = sort;
    }

    pub fn set_columns(&mut self, columns: Vec<GridColumn>) {
        self.columns = columns;
        self.widths.clear();
    }

    pub fn set_rows(&mut self, rows: Rows) {
        self.rows = rows;
        if self.widths.len() != self.columns.len() {
            self.widths = self.estimate_widths();
        }
    }

    /// Width from the header and the first rows' content.
    fn estimate_widths(&self) -> Vec<Pixels> {
        self.columns
            .iter()
            .enumerate()
            .map(|(ix, col)| {
                let header = col
                    .name
                    .chars()
                    .count()
                    .max(col.data_type.as_ref().map_or(0, |t| t.chars().count()));
                let content = self
                    .rows
                    .iter()
                    .take(100)
                    .map(|r| {
                        r.get(ix)
                            .and_then(|v| v.as_deref())
                            .map_or(4, |v| v.lines().next().unwrap_or("").chars().count())
                    })
                    .max()
                    .unwrap_or(0);
                let chars = header.max(content).min(48) as f32;
                px((chars * 7.5 + 40.).clamp(70., 380.))
            })
            .collect()
    }

    fn value(&self, row: usize, col: usize) -> Option<&str> {
        self.rows.get(row)?.get(col)?.as_deref()
    }

    /// The text copied for the current selection: one cell, or a row as
    /// tab-separated values.
    pub fn selection_text(&self, selection: TableSelection) -> Option<String> {
        let cell = |v: Option<&str>| v.unwrap_or("NULL").to_string();
        match selection {
            TableSelection::Cell(r, c) => Some(cell(self.value(r, c))),
            TableSelection::Row(r) => Some(
                (0..self.columns.len())
                    .map(|c| cell(self.value(r, c)))
                    .collect::<Vec<_>>()
                    .join("\t"),
            ),
            TableSelection::Column(c) => Some(
                (0..self.rows.len())
                    .map(|r| cell(self.value(r, c)))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            TableSelection::None => None,
        }
    }
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let col = &self.columns[col_ix];
        let width = self.widths.get(col_ix).copied().unwrap_or(px(120.));
        Column::new(SharedString::from(format!("c{col_ix}")), col.name.clone())
            .width(width)
            .min_width(px(48.))
            .when(col.numeric, |c| c.text_right())
            .when(self.on_sort.is_some(), |c| {
                c.sort(match self.sort {
                    Some((ix, true)) if ix == col_ix => ColumnSort::Ascending,
                    Some((ix, false)) if ix == col_ix => ColumnSort::Descending,
                    _ => ColumnSort::Default,
                })
            })
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let col = &self.columns[col_ix];
        v_flex()
            .w_full()
            .overflow_hidden()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .truncate()
                    .child(col.name.clone()),
            )
            .when_some(col.data_type.clone(), |d, t| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(t),
                )
            })
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let numeric = self.columns.get(col_ix).is_some_and(|c| c.numeric);
        let el = div()
            .w_full()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .when(numeric, |d| d.text_right().font_family("monospace"));
        match self.value(row_ix, col_ix) {
            None => el
                .italic()
                .text_color(cx.theme().muted_foreground.opacity(0.7))
                .child("NULL"),
            Some(v) => el.child(cell_preview(v, CELL_CHARS)),
        }
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        Some(self.value(row_ix, col_ix).unwrap_or("NULL").to_string().into())
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.value(row_ix, col_ix).unwrap_or("NULL").to_string()
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        _: ColumnSort,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        // The owner reloads and then updates this table, which must happen
        // after the table's own update has finished.
        if let Some(cb) = self.on_sort.clone() {
            window.defer(cx, move |window, cx| cb(col_ix, window, cx));
        }
    }

    fn loading(&self, _: &App) -> bool {
        self.loading && self.rows.is_empty()
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .gap_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(IconName::Inbox))
            .child(self.empty_text.clone())
    }
}

/// Build a grid state with the options every grid shares.
pub fn new_grid(
    delegate: GridDelegate,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TableState<GridDelegate>> {
    cx.new(|cx| {
        TableState::new(delegate, window, cx)
            .cell_selectable(true)
            .col_movable(false)
            .sortable(true)
    })
}

/// Copy the grid's selection to the clipboard.
pub fn copy_selection(table: &Entity<TableState<GridDelegate>>, cx: &mut App) {
    let state = table.read(cx);
    if let Some(text) = state.delegate().selection_text(state.selection()) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

/// Wrap a `DataTable` so Cmd/Ctrl+C copies the selection.
pub fn grid_element(table: &Entity<TableState<GridDelegate>>) -> impl IntoElement {
    let t = table.clone();
    div()
        .key_context(CONTEXT)
        .size_full()
        .on_action(move |_: &CopySelection, _, cx| copy_selection(&t, cx))
        .child(
            gpui_kit::component::table::DataTable::new(table)
                .stripe(true)
                .bordered(false)
                .header_row_height(px(42.)),
        )
}
