//! Integration tests against the fixture database from `docker-compose.yml`.
//!
//! Start it with `docker compose up -d`. Set `ZEBLE_TEST_URL` to use another
//! server; the tests are skipped when the server is unreachable.

use zeble_core::{
    ConnectionConfig, Db, SslMode,
    introspect::{ObjectKind, load_catalog, load_object_detail, load_relation},
    query::{count_rows, fetch_page},
    runtime,
    sql::{PageQuery, SortDir, StableOrder},
};

fn config() -> ConnectionConfig {
    let url = std::env::var("ZEBLE_TEST_URL")
        .unwrap_or_else(|_| "postgres://zeble:zeble@localhost:55432/zeble".into());
    let mut c = ConnectionConfig::default();
    c.apply_url(&url).unwrap();
    c.ssl_mode = SslMode::Disable;
    c
}

/// Run `f` against the fixture DB, or skip when it is not running.
fn with_db<F, Fut>(config: ConnectionConfig, f: F)
where
    F: FnOnce(Db) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    runtime().block_on(async move {
        match Db::connect(&config).await {
            Ok((db, _)) => f(db).await,
            Err(e) => eprintln!("skipping: fixture database unavailable: {e:#}"),
        }
    });
}

#[test]
fn catalog_lists_every_kind() {
    with_db(config(), |db| async move {
        let client = db.client().await.unwrap();
        let catalog = load_catalog(&**client).await.unwrap();
        assert!(catalog.schemas.contains(&"public".to_string()));
        assert!(catalog.schemas.contains(&"shop".to_string()));
        let find = |schema: &str, name: &str| {
            catalog
                .objects
                .iter()
                .find(|o| o.schema == schema && o.name == name)
                .unwrap_or_else(|| panic!("{schema}.{name} missing"))
                .kind
        };
        assert_eq!(find("public", "users"), ObjectKind::Table);
        assert_eq!(find("public", "event_log"), ObjectKind::Table);
        assert_eq!(find("public", "active_admins"), ObjectKind::View);
        assert_eq!(
            find("shop", "revenue_by_status"),
            ObjectKind::MaterializedView
        );
        assert_eq!(find("public", "user_order_count"), ObjectKind::Function);
        assert_eq!(find("shop", "cancel_order"), ObjectKind::Procedure);
        assert_eq!(find("public", "invoice_no"), ObjectKind::Sequence);
        assert_eq!(find("shop", "order_status"), ObjectKind::Enum);
        assert_eq!(find("shop", "email"), ObjectKind::Domain);
        assert_eq!(find("shop", "address"), ObjectKind::Composite);
        assert!(!catalog.objects.iter().any(|o| o.schema == "pg_catalog"));
        // Row types of tables are not listed as composite types.
        assert!(
            !catalog
                .objects
                .iter()
                .any(|o| o.kind == ObjectKind::Composite && o.name == "users")
        );
    });
}

#[test]
fn relation_structure_and_paging() {
    with_db(config(), |db| async move {
        let client = db.client().await.unwrap();
        let catalog = load_catalog(&**client).await.unwrap();
        let users = catalog.objects.iter().find(|o| o.name == "users").unwrap();
        let info = load_relation(&**client, users.oid).await.unwrap();
        assert_eq!(info.primary_key, vec!["id".to_string()]);
        assert_eq!(info.columns[0].name, "id");
        assert!(info.columns[0].primary_key);
        assert!(info.columns[0].is_numeric());
        assert!(
            !info
                .columns
                .iter()
                .find(|c| c.name == "email")
                .unwrap()
                .nullable
        );
        assert!(info.indexes.iter().any(|i| i.primary));
        assert!(info.constraints.iter().any(|c| c.kind == "UNIQUE"));
        assert_eq!(info.comment.as_deref(), Some("Application users"));

        let q = PageQuery {
            schema: "public".into(),
            name: "users".into(),
            sort: Some(("id".into(), SortDir::Desc)),
            stable: StableOrder::PrimaryKey(info.primary_key.clone()),
            page: 1,
            page_size: 500,
        };
        let page = fetch_page(&client, &q).await.unwrap();
        assert_eq!(page.rows.len(), 500);
        assert_eq!(page.columns[0], "id");
        assert_eq!(page.rows[0][0].as_deref(), Some("1845"));
        // NULL names (every 7th user) arrive as None, bytea as hex text.
        let all = fetch_page(
            &client,
            &PageQuery {
                page: 0,
                page_size: 5000,
                sort: None,
                ..q
            },
        )
        .await
        .unwrap();
        assert!(all.rows.iter().any(|r| r[2].is_none()));
        assert!(
            all.rows
                .iter()
                .any(|r| r[5].as_deref().is_some_and(|v| v.starts_with("\\x")))
        );
        assert_eq!(count_rows(&client, "public", "users").await.unwrap(), 2345);

        let orders = catalog.objects.iter().find(|o| o.name == "orders").unwrap();
        let info = load_relation(&**client, orders.oid).await.unwrap();
        assert!(
            info.constraints
                .iter()
                .any(|c| c.kind == "FOREIGN KEY" && c.definition.contains("REFERENCES users(id)"))
        );

        let log = catalog
            .objects
            .iter()
            .find(|o| o.name == "event_log")
            .unwrap();
        let info = load_relation(&**client, log.oid).await.unwrap();
        assert!(info.primary_key.is_empty() && info.has_ctid);

        let view = catalog
            .objects
            .iter()
            .find(|o| o.name == "active_admins")
            .unwrap();
        let info = load_relation(&**client, view.oid).await.unwrap();
        assert!(!info.has_ctid);
        assert!(
            info.view_definition
                .unwrap()
                .starts_with("CREATE VIEW public.active_admins AS")
        );
    });
}

#[test]
fn object_details() {
    with_db(config(), |db| async move {
        let client = db.client().await.unwrap();
        let catalog = load_catalog(&**client).await.unwrap();
        let get = |name: &str| {
            catalog
                .objects
                .iter()
                .find(|o| o.name == name)
                .unwrap()
                .clone()
        };

        let f = load_object_detail(&**client, &get("user_order_count"))
            .await
            .unwrap();
        assert!(
            f.definition
                .contains("CREATE OR REPLACE FUNCTION public.user_order_count")
        );
        let e = load_object_detail(&**client, &get("order_status"))
            .await
            .unwrap();
        assert!(e.definition.contains("'pending'"));
        let s = load_object_detail(&**client, &get("invoice_no"))
            .await
            .unwrap();
        assert!(s.definition.contains("INCREMENT BY 5"));
        let d = load_object_detail(&**client, &get("email")).await.unwrap();
        assert!(d.definition.starts_with("CREATE DOMAIN shop.email AS text"));
        let c = load_object_detail(&**client, &get("address"))
            .await
            .unwrap();
        assert!(c.definition.contains("street text"));
    });
}

#[test]
fn query_session_statements_transactions_errors() {
    with_db(config(), |db| async move {
        let s = db.open_session().await.unwrap();
        let out = s.run("SELECT 1 AS a; SELECT 'x' AS b, NULL AS c; CREATE TEMP TABLE t(x int); INSERT INTO t VALUES (1),(2)").await;
        assert!(out.error.is_none());
        assert_eq!(out.statements.len(), 4);
        assert_eq!(out.statements[3].row_count, 2);
        let last = out.last_result().unwrap().result.as_ref().unwrap();
        assert_eq!(last.columns, ["b", "c"]);
        assert_eq!(last.rows, vec![vec![Some("x".to_string()), None]]);
        assert!(!out.in_transaction);

        // Session state persists between runs on the same session.
        let out = s.run("BEGIN; INSERT INTO t VALUES (3)").await;
        assert!(out.in_transaction);
        let out = s.run("SELCT oops").await;
        let err = out.error.unwrap();
        assert_eq!(err.position, Some(0));
        assert!(
            out.in_transaction,
            "aborted transaction still counts as open"
        );
        s.rollback().await.unwrap();
        let out = s.run("SELECT count(*) FROM t").await;
        assert_eq!(
            out.last_result().unwrap().result.as_ref().unwrap().rows[0][0].as_deref(),
            Some("2")
        );
        assert!(!out.in_transaction);

        // Results are capped.
        let out = s.run("SELECT g FROM generate_series(1, 10050) g").await;
        let st = out.last_result().unwrap();
        assert!(st.truncated);
        assert_eq!(st.result.as_ref().unwrap().rows.len(), 10_000);
        assert_eq!(st.row_count, 10_050);

        // Error positions point into the sent SQL.
        let out = s.run("SELECT 1;\nSELECT * FROM nope").await;
        assert_eq!(out.statements.len(), 1);
        let err = out.error.unwrap();
        assert_eq!(err.code.as_deref(), Some("42P01"));
        assert_eq!(err.position, Some(24));
    });
}

#[test]
fn cancel_running_query() {
    with_db(config(), |db| async move {
        let s = db.open_session().await.unwrap();
        let canceller = s.canceller();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            canceller.cancel();
        });
        let out = s.run("SELECT pg_sleep(10)").await;
        assert_eq!(out.error.unwrap().code.as_deref(), Some("57014"));
    });
}

#[test]
fn read_only_connections_reject_writes() {
    let mut c = config();
    c.read_only = true;
    with_db(c, |db| async move {
        let s = db.open_session().await.unwrap();
        let out = s
            .run("CREATE TEMP TABLE x(a int); INSERT INTO public.event_log(kind) VALUES ('x')")
            .await;
        assert_eq!(out.error.unwrap().code.as_deref(), Some("25006"));
    });
}

#[test]
fn bad_credentials_give_a_readable_error() {
    let mut c = config();
    c.password = "wrong".into();
    runtime().block_on(async move {
        if Db::connect(&config()).await.is_err() {
            return; // fixture not running
        }
        let err = Db::test(&c).await.unwrap_err().to_string();
        assert!(err.contains("password authentication failed"), "{err}");
    });
}

#[test]
fn per_statement_timing() {
    with_db(config(), |db| async move {
        let s = db.open_session().await.unwrap();
        let out = s.run("SELECT 1; SELECT pg_sleep(0.4); SELECT 2").await;
        let t: Vec<_> = out
            .statements
            .iter()
            .map(|s| s.elapsed.as_millis())
            .collect();
        eprintln!("timings: {t:?}");
        assert!(t[0] < 200, "{t:?}");
        assert!(t[1] >= 350, "{t:?}");
        assert!(t[2] < 200, "{t:?}");
    });
}
