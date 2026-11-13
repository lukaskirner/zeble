//! Per-window connection state: the pool, connection status, the catalog,
//! and the list of running operations shown in the top bar.

use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use gpui_kit::{Context, EventEmitter, SharedString, Task};
use parking_lot::Mutex;
use zeble_core::{
    Canceller, ConnectionConfig, Db,
    db::PooledClient,
    introspect::{Catalog, load_catalog},
    is_connection_lost, pooled_canceller,
};

use crate::util::{on_db, on_db_abortable};

#[derive(Clone, Debug, PartialEq)]
pub enum ConnStatus {
    Connecting,
    Connected { version: SharedString },
    Disconnected(SharedString),
    Error(SharedString),
}

/// Where a running operation's canceller lands once the DB task has a connection.
pub type CancelSlot = Arc<Mutex<Option<Canceller>>>;

pub struct Activity {
    pub id: u64,
    pub label: SharedString,
    pub started: Instant,
    pub cancel: CancelSlot,
}

pub enum SessionEvent {
    /// The pool was (re)created: views reload their data.
    Connected,
    CatalogChanged,
}

pub struct Session {
    pub config: ConnectionConfig,
    db: Option<Db>,
    pub status: ConnStatus,
    pub activities: Vec<Activity>,
    next_activity: u64,
    ticker: Option<Task<()>>,
    pub catalog: Option<Arc<Catalog>>,
    pub catalog_error: Option<SharedString>,
    pub catalog_loading: bool,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    pub fn new(config: ConnectionConfig, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            config,
            db: None,
            status: ConnStatus::Connecting,
            activities: Vec::new(),
            next_activity: 0,
            ticker: None,
            catalog: None,
            catalog_error: None,
            catalog_loading: false,
        };
        this.connect(cx);
        this
    }

    pub fn db(&self) -> Option<Db> {
        self.db.clone()
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.status, ConnStatus::Connected { .. })
    }

    /// (Re)connect: build a new pool, then load the catalog.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        if let Some(old) = self.db.take() {
            old.close();
        }
        self.status = ConnStatus::Connecting;
        let config = self.config.clone();
        cx.spawn(async move |this, cx| {
            // 1. Run the password command, if any (Cancel kills it).
            let password = match config.password_command() {
                None => Ok(config.password.clone()),
                Some(cmd) => {
                    let cmd = cmd.to_string();
                    let (work, abort) = on_db_abortable(async move {
                        zeble_core::secret::run_password_command(&cmd).await
                    });
                    let slot = Arc::new(Mutex::new(Some(Canceller::abort_task(abort))));
                    let id = this
                        .update(cx, |s, cx| s.begin("Running password command…", slot, cx))
                        .ok();
                    let result = work.await.and_then(|r| r);
                    if let Some(id) = id {
                        this.update(cx, |s, cx| s.end(id, cx)).ok();
                    }
                    result
                }
            };
            // 2. Build the pool and open the first connection.
            let result = match password {
                Ok(password) => {
                    let config = config.clone();
                    let (work, abort) =
                        on_db_abortable(async move { Db::connect(&config, &password).await });
                    let slot = Arc::new(Mutex::new(Some(Canceller::abort_task(abort))));
                    let id = this
                        .update(cx, |s, cx| s.begin("Connecting…", slot, cx))
                        .ok();
                    let result = work.await.and_then(|r| r);
                    if let Some(id) = id {
                        this.update(cx, |s, cx| s.end(id, cx)).ok();
                    }
                    result
                }
                Err(e) => Err(e),
            };
            this.update(cx, |this, cx| {
                match result {
                    Ok((db, version)) => {
                        this.db = Some(db);
                        // "17.2 (Debian 17.2-1.pgdg120+1)" -> "17.2"
                        let short = version.split(" (").next().unwrap_or(&version).to_string();
                        this.status = ConnStatus::Connected {
                            version: short.into(),
                        };
                        cx.emit(SessionEvent::Connected);
                        this.load_catalog(cx);
                    }
                    Err(e) => {
                        this.status = ConnStatus::Error(zeble_core::db::error_text(&e).into())
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn load_catalog(&mut self, cx: &mut Context<Self>) {
        self.catalog_loading = true;
        let task = self.with_client("Loading schema…", cx, |client| async move {
            load_catalog(&**client).await
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.catalog_loading = false;
                match result {
                    Ok(catalog) => {
                        this.catalog = Some(Arc::new(catalog));
                        this.catalog_error = None;
                    }
                    Err(e) => this.catalog_error = Some(zeble_core::db::error_text(&e).into()),
                }
                cx.emit(SessionEvent::CatalogChanged);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Register a running operation for the top bar.
    pub fn begin(
        &mut self,
        label: impl Into<SharedString>,
        cancel: CancelSlot,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.next_activity += 1;
        let id = self.next_activity;
        self.activities.push(Activity {
            id,
            label: label.into(),
            started: Instant::now(),
            cancel,
        });
        // Re-render once in a while so elapsed times tick.
        if self.ticker.is_none() {
            self.ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(200))
                        .await;
                    let keep = this
                        .update(cx, |this, cx| {
                            cx.notify();
                            if this.activities.is_empty() {
                                this.ticker = None;
                                false
                            } else {
                                true
                            }
                        })
                        .unwrap_or(false);
                    if !keep {
                        break;
                    }
                }
            }));
        }
        cx.notify();
        id
    }

    pub fn end(&mut self, id: u64, cx: &mut Context<Self>) {
        self.activities.retain(|a| a.id != id);
        cx.notify();
    }

    /// Ask the server to cancel every running operation that can be cancelled.
    pub fn cancel_all(&mut self) {
        for a in &self.activities {
            if let Some(c) = a.cancel.lock().as_ref() {
                c.cancel();
            }
        }
    }

    pub fn can_cancel(&self) -> bool {
        self.activities.iter().any(|a| a.cancel.lock().is_some())
    }

    /// Mark the connection as dropped when an error says so.
    pub fn note_error(&mut self, err: &anyhow::Error, cx: &mut Context<Self>) {
        if is_connection_lost(err) && self.is_connected() {
            self.status = ConnStatus::Disconnected(zeble_core::db::error_text(err).into());
            cx.notify();
        }
    }

    /// Run `f` with a pooled connection on the DB runtime, tracked as an
    /// activity labelled `label`.
    pub fn with_client<T, F, Fut>(
        &mut self,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
        f: F,
    ) -> Task<anyhow::Result<T>>
    where
        T: Send + 'static,
        F: FnOnce(PooledClient) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let Some(db) = self.db.clone() else {
            return Task::ready(Err(anyhow!("Not connected")));
        };
        let slot: CancelSlot = Arc::new(Mutex::new(None));
        let id = self.begin(label, slot.clone(), cx);
        let work = on_db(async move {
            let client = db.client().await?;
            *slot.lock() = Some(pooled_canceller(&client));
            let result = f(client).await;
            slot.lock().take();
            result
        });
        cx.spawn(async move |this, cx| {
            let result = work.await.and_then(|r| r);
            this.update(cx, |this, cx| {
                this.end(id, cx);
                if let Err(e) = &result {
                    this.note_error(e, cx);
                }
            })
            .ok();
            result
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(db) = self.db.take() {
            db.close();
        }
    }
}
