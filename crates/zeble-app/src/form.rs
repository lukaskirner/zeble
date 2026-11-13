//! The add/edit connection form shown beside the connection list.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    select::{Select, SelectState},
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeble_core::{ColorTag, ConnectionConfig, Db, SslMode};

use crate::util::{color_tag, on_db};

pub enum FormEvent {
    Saved(ConnectionConfig),
    Cancelled,
}

enum TestState {
    Idle,
    Testing,
    Ok(SharedString),
    Failed(SharedString),
}

pub struct ConnectionForm {
    /// The connection as last loaded, so unedited fields (id) carry over.
    base: ConnectionConfig,
    pub is_new: bool,
    url: Entity<InputState>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    password_command: Entity<InputState>,
    /// Password mode: the command field is used instead of the password.
    use_command: bool,
    database: Entity<InputState>,
    ssl: Entity<SelectState<Vec<SharedString>>>,
    color: Option<ColorTag>,
    read_only: bool,
    test: TestState,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<FormEvent> for ConnectionForm {}

fn ssl_index(mode: SslMode) -> usize {
    SslMode::ALL.iter().position(|m| *m == mode).unwrap_or(1)
}

impl ConnectionForm {
    pub fn new(
        config: ConnectionConfig,
        is_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input =
            |placeholder: &str, value: String, window: &mut Window, cx: &mut Context<Self>| {
                let placeholder = SharedString::from(placeholder.to_string());
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(value)
                })
            };
        let url = input(
            "postgres://user:password@host:5432/database",
            String::new(),
            window,
            cx,
        );
        let name = input("My database", config.name.clone(), window, cx);
        let host = input("localhost", config.host.clone(), window, cx);
        let port = input("5432", config.port.to_string(), window, cx);
        let user = input("postgres", config.user.clone(), window, cx);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Password")
                .masked(true)
                .default_value(config.password.clone())
        });
        let password_command = input(
            "op read op://Private/database/password",
            config.password_command.clone().unwrap_or_default(),
            window,
            cx,
        );
        let database = input("postgres", config.database.clone(), window, cx);
        let ssl = cx.new(|cx| {
            SelectState::new(
                SslMode::ALL
                    .iter()
                    .map(|m| SharedString::from(m.as_str()))
                    .collect::<Vec<_>>(),
                Some(IndexPath::default().row(ssl_index(config.ssl_mode))),
                window,
                cx,
            )
        });
        let subscriptions = vec![cx.subscribe_in(&url, window, |this, _, ev, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.apply_url(window, cx);
            }
        })];
        Self {
            use_command: config.password_command().is_some(),
            color: config.color,
            read_only: config.read_only,
            base: config,
            is_new,
            url,
            name,
            host,
            port,
            user,
            password,
            password_command,
            database,
            ssl,
            test: TestState::Idle,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn id(&self) -> &str {
        &self.base.id
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.name.update(cx, |s, cx| s.focus(window, cx));
    }

    /// The form's current values as a config.
    fn collect(&self, cx: &App) -> anyhow::Result<ConnectionConfig> {
        let text = |s: &Entity<InputState>| s.read(cx).value().trim().to_string();
        let port_text = text(&self.port);
        let port = if port_text.is_empty() {
            5432
        } else {
            port_text
                .parse::<u16>()
                .map_err(|_| anyhow::anyhow!("Port must be a number between 1 and 65535"))?
        };
        let ssl_mode = self
            .ssl
            .read(cx)
            .selected_value()
            .and_then(|v| SslMode::parse(v))
            .unwrap_or_default();
        let config = ConnectionConfig {
            id: self.base.id.clone(),
            name: text(&self.name),
            host: text(&self.host),
            port,
            user: text(&self.user),
            // Passwords may legitimately have surrounding spaces.
            password: if self.use_command {
                String::new()
            } else {
                self.password.read(cx).value().to_string()
            },
            password_command: if self.use_command {
                let cmd = text(&self.password_command);
                if cmd.is_empty() {
                    anyhow::bail!("Enter the command that prints the password");
                }
                Some(cmd)
            } else {
                None
            },
            database: text(&self.database),
            ssl_mode,
            color: self.color,
            read_only: self.read_only,
        };
        config.validate()?;
        Ok(config)
    }

    fn set_fields(&mut self, c: &ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        let set =
            |s: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| {
                s.update(cx, |s, cx| s.set_value(v, window, cx));
            };
        set(&self.host, c.host.clone(), window, cx);
        set(&self.port, c.port.to_string(), window, cx);
        set(&self.user, c.user.clone(), window, cx);
        set(&self.password, c.password.clone(), window, cx);
        set(&self.database, c.database.clone(), window, cx);
        self.ssl.update(cx, |s, cx| {
            s.set_selected_index(
                Some(IndexPath::default().row(ssl_index(c.ssl_mode))),
                window,
                cx,
            )
        });
    }

    fn apply_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.url.read(cx).value().to_string();
        if url.trim().is_empty() {
            return;
        }
        let mut config = self.collect(cx).unwrap_or_else(|_| self.base.clone());
        match config.apply_url(&url) {
            Ok(had_password) => {
                self.set_fields(&config, window, cx);
                if had_password {
                    self.use_command = false;
                }
                self.url.update(cx, |s, cx| s.set_value("", window, cx));
                self.error = None;
            }
            Err(e) => self.error = Some(zeble_core::db::error_text(&e).into()),
        }
        cx.notify();
    }

    fn test_connection(&mut self, cx: &mut Context<Self>) {
        let config = match self.collect(cx) {
            Ok(c) => c,
            Err(e) => {
                self.test = TestState::Failed(zeble_core::db::error_text(&e).into());
                cx.notify();
                return;
            }
        };
        self.test = TestState::Testing;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = on_db(async move { Db::test(&config).await }).await;
            this.update(cx, |this, cx| {
                this.test = match result.and_then(|r| r) {
                    Ok(v) => TestState::Ok(
                        format!(
                            "Connected · PostgreSQL {}",
                            v.split(" (").next().unwrap_or(&v)
                        )
                        .into(),
                    ),
                    Err(e) => TestState::Failed(zeble_core::db::error_text(&e).into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        match self.collect(cx) {
            Ok(config) => {
                self.error = None;
                self.base = config.clone();
                self.is_new = false;
                cx.emit(FormEvent::Saved(config));
            }
            Err(e) => self.error = Some(zeble_core::db::error_text(&e).into()),
        }
        cx.notify();
    }

    fn field(label: &'static str, input: impl IntoElement) -> Div {
        v_flex()
            .gap_1()
            .flex_1()
            .child(Label::new(label).text_sm())
            .child(input)
    }

    /// The password field with its Password | Command switch.
    fn render_password(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        v_flex()
            .gap_1()
            .flex_1()
            .child(
                h_flex()
                    .justify_between()
                    .child(Label::new("Password").text_sm())
                    .child(
                        TabBar::new("password-mode")
                            .segmented()
                            .xsmall()
                            .selected_index(if self.use_command { 1 } else { 0 })
                            .child(Tab::new().label("Password"))
                            .child(Tab::new().label("Command"))
                            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                                this.use_command = *ix == 1;
                                let field = if this.use_command {
                                    &this.password_command
                                } else {
                                    &this.password
                                };
                                field.update(cx, |s, cx| s.focus(window, cx));
                                cx.notify();
                            })),
                    ),
            )
            .child(if self.use_command {
                Input::new(&self.password_command)
                    .prefix(Icon::new(IconName::SquareTerminal).xsmall())
                    .into_any_element()
            } else {
                Input::new(&self.password).mask_toggle().into_any_element()
            })
    }

    fn render_colors(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let swatch = |id: &'static str, tag: Option<ColorTag>, cx: &mut Context<Self>| {
            let selected = self.color == tag;
            let theme = cx.theme();
            div()
                .id(id)
                .size_6()
                .rounded_full()
                .border_2()
                .cursor_pointer()
                .border_color(if selected {
                    theme.foreground
                } else {
                    theme.border
                })
                .map(|d| match tag {
                    Some(t) => d.bg(color_tag(t)),
                    None => d.bg(theme.background).child(
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("–"),
                    ),
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.color = tag;
                    cx.notify();
                }))
        };
        let mut row = h_flex().gap_2().child(swatch("color-none", None, cx));
        for (i, tag) in ColorTag::ALL.iter().enumerate() {
            const IDS: [&str; 7] = ["c0", "c1", "c2", "c3", "c4", "c5", "c6"];
            row = row.child(swatch(IDS[i], Some(*tag), cx));
        }
        row
    }
}

impl Render for ConnectionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let testing = matches!(self.test, TestState::Testing);
        let fields = v_flex()
            .id("connection-form")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_6()
            .gap_4()
            .child(
                h_flex().justify_between().child(
                    Label::new(if self.is_new {
                        "New Connection"
                    } else {
                        "Edit Connection"
                    })
                    .text_lg()
                    .font_semibold(),
                ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .p_3()
                    .rounded(theme.radius)
                    .bg(theme.muted.opacity(0.5))
                    .child(Label::new("Paste a connection URL to fill in the fields").text_sm())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.url).small()))
                            .child(
                                Button::new("apply-url")
                                    .small()
                                    .outline()
                                    .label("Fill")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.apply_url(window, cx)
                                    })),
                            ),
                    ),
            )
            .child(Self::field("Name", Input::new(&self.name)))
            .child(
                h_flex()
                    .gap_3()
                    .child(Self::field("Host", Input::new(&self.host)).flex_grow(1.))
                    .child(
                        Self::field("Port", Input::new(&self.port))
                            .w_24()
                            .flex_none(),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(Self::field("User", Input::new(&self.user)))
                    .child(Self::field("Database", Input::new(&self.database))),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(self.render_password(cx))
                    .child(
                        Self::field("SSL mode", Select::new(&self.ssl))
                            .w_40()
                            .flex_none(),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new("Color tag").text_sm())
                    .child(self.render_colors(cx)),
            )
            .child(
                Checkbox::new("read-only")
                    .label("Read-only: block writes for this connection")
                    .checked(self.read_only)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.read_only = *checked;
                        cx.notify();
                    })),
            )
            .child(
                Label::new(if self.use_command {
                    "The command runs in your login shell ($SHELL -l -c) when you connect. \
                     Its output is the password and is never saved."
                } else {
                    "The password is stored in plain text in Zeble's connections file."
                })
                .text_xs()
                .text_color(theme.muted_foreground),
            )
            .when_some(self.error.clone(), |this, err| {
                this.child(Label::new(err).text_sm().text_color(theme.danger))
            })
            .map(|this| match &self.test {
                TestState::Idle | TestState::Testing => this,
                TestState::Ok(msg) => {
                    this.child(Label::new(msg.clone()).text_sm().text_color(theme.success))
                }
                TestState::Failed(msg) => {
                    this.child(Label::new(msg.clone()).text_sm().text_color(theme.danger))
                }
            });
        v_flex().size_full().child(fields).child(
            h_flex()
                .gap_2()
                .px_6()
                .py_3()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    Button::new("test")
                        .outline()
                        .label("Test Connection")
                        .icon(IconName::Globe)
                        .loading(testing)
                        .disabled(testing)
                        .on_click(cx.listener(|this, _, _, cx| this.test_connection(cx))),
                )
                .child(div().flex_1())
                .child(
                    Button::new("cancel")
                        .ghost()
                        .label("Cancel")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(FormEvent::Cancelled))),
                )
                .child(
                    Button::new("save")
                        .primary()
                        .label("Save")
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                ),
        )
    }
}
