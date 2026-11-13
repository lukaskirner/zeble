//! A tab for functions, procedures, sequences and types: properties plus
//! the SQL definition in a read-only editor.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeble_core::introspect::{DbObject, ObjectDetail, load_object_detail};

use crate::{
    relation_tab::{section, simple_table},
    session::{Session, SessionEvent},
};

pub struct ObjectTab {
    session: Entity<Session>,
    pub object: DbObject,
    detail: Option<ObjectDetail>,
    error: Option<SharedString>,
    loading: bool,
    editor: Entity<EditorState>,
    /// Set when new SQL arrived; applied in render, which has a `Window`.
    set_editor_pending: bool,
    _subscriptions: Vec<Subscription>,
}

impl ObjectTab {
    pub fn new(
        session: Entity<Session>,
        object: DbObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| EditorState::new(window, cx).language("sql").line_number(true));
        let subscriptions = vec![cx.subscribe(&session, |this, _, ev, cx| {
            if let SessionEvent::Connected = ev {
                this.reload(cx);
            }
        })];
        let mut this = Self {
            session,
            object,
            detail: None,
            error: None,
            loading: false,
            editor,
            set_editor_pending: false,
            _subscriptions: subscriptions,
        };
        this.reload(cx);
        this
    }

    pub fn title(&self) -> String {
        self.object.display_name()
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let obj = self.object.clone();
        self.loading = true;
        let task = self.session.update(cx, |s, cx| {
            s.with_client(
                format!("Loading {}…", obj.qualified_name()),
                cx,
                move |client| async move { load_object_detail(&**client, &obj).await },
            )
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(detail) => {
                        this.error = None;
                        this.detail = Some(detail);
                        this.set_editor_pending = true;
                    }
                    Err(e) => this.error = Some(format!("{e:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for ObjectTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if self.set_editor_pending {
            self.set_editor_pending = false;
            let sql = self
                .detail
                .as_ref()
                .map(|d| d.definition.clone())
                .unwrap_or_default();
            self.editor.update(cx, |e, cx| e.set_value(sql, window, cx));
        }
        let props = self
            .detail
            .as_ref()
            .map(|d| {
                d.properties
                    .iter()
                    .map(|(k, v)| vec![k.clone(), v.clone()])
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h_10()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .truncate()
                            .child(format!(
                                "{}.{}",
                                self.object.schema,
                                self.object.display_name()
                            )),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(self.object.kind.label()),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("reload")
                            .xsmall()
                            .ghost()
                            .icon(Lucide::RefreshCw)
                            .tooltip("Reload")
                            .loading(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .when_some(self.error.clone(), |d, err| {
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
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .gap_5()
                    .child(section(
                        "Properties",
                        simple_table(&[("Property", 2.), ("Value", 6.)], props, cx),
                    ))
                    .child(
                        section(
                            "Definition",
                            div()
                                .size_full()
                                .child(Editor::new(&self.editor).readonly(true).h_full()),
                        )
                        .flex_1()
                        .min_h_0(),
                    ),
            )
    }
}
