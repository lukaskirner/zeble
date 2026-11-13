//! Catalog queries: the object tree for the sidebar and per-object details.

use tokio_postgres::GenericClient;

use crate::db::pg_err;
use crate::sql::{display_qualified, quote_ident_min as quote_ident, quote_literal};

/// Schemas hidden from the sidebar.
const SYSTEM_SCHEMA_FILTER: &str = "n.nspname NOT IN ('pg_catalog', 'information_schema') \
     AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp\\_%'";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    ForeignTable,
    Function,
    Procedure,
    Sequence,
    Enum,
    Domain,
    Composite,
    Range,
}

impl ObjectKind {
    /// Whether the object has rows to browse (Data / Structure tabs).
    pub fn is_relation(self) -> bool {
        matches!(
            self,
            ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::MaterializedView
                | ObjectKind::ForeignTable
        )
    }

    /// Plural group label used in the sidebar.
    pub fn group_label(self) -> &'static str {
        match self {
            ObjectKind::Table => "Tables",
            ObjectKind::View => "Views",
            ObjectKind::MaterializedView => "Materialized Views",
            ObjectKind::ForeignTable => "Foreign Tables",
            ObjectKind::Function => "Functions",
            ObjectKind::Procedure => "Procedures",
            ObjectKind::Sequence => "Sequences",
            ObjectKind::Enum | ObjectKind::Domain | ObjectKind::Composite | ObjectKind::Range => {
                "Types"
            }
        }
    }

    /// The order groups appear in under a schema.
    pub fn group_order(self) -> u8 {
        match self {
            ObjectKind::Table => 0,
            ObjectKind::View => 1,
            ObjectKind::MaterializedView => 2,
            ObjectKind::ForeignTable => 3,
            ObjectKind::Function => 4,
            ObjectKind::Procedure => 5,
            ObjectKind::Sequence => 6,
            _ => 7,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ObjectKind::Table => "table",
            ObjectKind::View => "view",
            ObjectKind::MaterializedView => "materialized view",
            ObjectKind::ForeignTable => "foreign table",
            ObjectKind::Function => "function",
            ObjectKind::Procedure => "procedure",
            ObjectKind::Sequence => "sequence",
            ObjectKind::Enum => "enum",
            ObjectKind::Domain => "domain",
            ObjectKind::Composite => "composite type",
            ObjectKind::Range => "range type",
        }
    }
}

/// One object in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbObject {
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    pub oid: u32,
    /// Argument list for functions and procedures, e.g. `uid bigint`.
    pub args: Option<String>,
}

impl DbObject {
    /// A stable key, unique within a database (overloads differ by args).
    pub fn key(&self) -> String {
        format!("{:?}:{}", self.kind, self.oid)
    }

    /// Name as shown in the sidebar; functions carry their arguments.
    pub fn display_name(&self) -> String {
        match &self.args {
            Some(args) => format!("{}({})", self.name, args),
            None => self.name.clone(),
        }
    }

    /// `schema.name`, quoted where needed.
    pub fn qualified_name(&self) -> String {
        display_qualified(&self.schema, &self.name)
    }
}

/// Everything the sidebar shows.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// All user schemas, including empty ones.
    pub schemas: Vec<String>,
    /// Sorted by schema, then kind group, then name.
    pub objects: Vec<DbObject>,
}

pub async fn load_catalog(client: &impl GenericClient) -> anyhow::Result<Catalog> {
    let schemas = client
        .query(
            &format!(
                "SELECT n.nspname FROM pg_namespace n WHERE {SYSTEM_SCHEMA_FILTER} ORDER BY 1"
            ),
            &[],
        )
        .await
        .map_err(pg_err)?
        .iter()
        .map(|r| r.get::<_, String>(0))
        .collect();

    let not_extension = "NOT EXISTS (SELECT 1 FROM pg_depend d \
         WHERE d.objid = {OID} AND d.deptype = 'e')";
    let sql = format!(
        "SELECT n.nspname, c.relname, c.relkind::text, c.oid, NULL::text \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind IN ('r','p','v','m','f','S') AND {SYSTEM_SCHEMA_FILTER} AND {ext_c} \
         UNION ALL \
         SELECT n.nspname, p.proname, 'proc_' || p.prokind::text, p.oid, \
                pg_get_function_identity_arguments(p.oid) \
         FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
         WHERE p.prokind IN ('f','p') AND {SYSTEM_SCHEMA_FILTER} AND {ext_p} \
         UNION ALL \
         SELECT n.nspname, t.typname, 't' || t.typtype::text, t.oid, NULL::text \
         FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
         WHERE t.typtype IN ('e','d','r','c') \
           AND (t.typtype <> 'c' OR (SELECT relkind FROM pg_class WHERE oid = t.typrelid) = 'c') \
           AND {SYSTEM_SCHEMA_FILTER} AND {ext_t}",
        ext_c = not_extension.replace("{OID}", "c.oid"),
        ext_p = not_extension.replace("{OID}", "p.oid"),
        ext_t = not_extension.replace("{OID}", "t.oid"),
    );
    let rows = client.query(&sql, &[]).await.map_err(pg_err)?;
    let mut objects: Vec<DbObject> = rows
        .iter()
        .filter_map(|r| {
            let kind = match r.get::<_, &str>(2) {
                "r" | "p" => ObjectKind::Table,
                "v" => ObjectKind::View,
                "m" => ObjectKind::MaterializedView,
                "f" => ObjectKind::ForeignTable,
                "S" => ObjectKind::Sequence,
                "proc_f" => ObjectKind::Function,
                "proc_p" => ObjectKind::Procedure,
                "te" => ObjectKind::Enum,
                "td" => ObjectKind::Domain,
                "tc" => ObjectKind::Composite,
                "tr" => ObjectKind::Range,
                _ => return None,
            };
            Some(DbObject {
                schema: r.get(0),
                name: r.get(1),
                kind,
                oid: r.get(3),
                args: r.get(4),
            })
        })
        .collect();
    objects.sort_by(|a, b| {
        (&a.schema, a.kind.group_order(), &a.name, &a.args).cmp(&(
            &b.schema,
            b.kind.group_order(),
            &b.name,
            &b.args,
        ))
    });
    Ok(Catalog { schemas, objects })
}

/// A column of a table, view or composite type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    /// Formatted type, e.g. `numeric(10,2)`.
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary_key: bool,
    pub comment: Option<String>,
    /// `pg_type.typcategory`: `N` numeric, `B` boolean, `S` string, ...
    pub category: char,
}

impl ColumnInfo {
    pub fn is_numeric(&self) -> bool {
        self.category == 'N'
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub definition: String,
    pub unique: bool,
    pub primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintInfo {
    pub name: String,
    /// "PRIMARY KEY", "FOREIGN KEY", "UNIQUE", "CHECK", "EXCLUDE".
    pub kind: String,
    pub definition: String,
}

/// Everything the Structure tab shows for a table or view.
#[derive(Debug, Clone, Default)]
pub struct RelationInfo {
    pub columns: Vec<ColumnInfo>,
    /// Primary key columns in key order (empty when there is none).
    pub primary_key: Vec<String>,
    pub indexes: Vec<IndexInfo>,
    pub constraints: Vec<ConstraintInfo>,
    /// `CREATE VIEW ... AS` text for views and materialized views.
    pub view_definition: Option<String>,
    pub comment: Option<String>,
    /// `pg_class.reltuples`; `None` when the table was never analyzed.
    pub estimated_rows: Option<u64>,
    /// Whether rows have a `ctid` (tables and materialized views do, views do not).
    pub has_ctid: bool,
}

pub async fn load_relation(client: &impl GenericClient, oid: u32) -> anyhow::Result<RelationInfo> {
    let columns = client
        .query(
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
                    pg_get_expr(d.adbin, d.adrelid), \
                    COALESCE(a.attnum = ANY(i.indkey), false), \
                    col_description(a.attrelid, a.attnum), t.typcategory::text \
             FROM pg_attribute a \
             JOIN pg_type t ON t.oid = a.atttypid \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             LEFT JOIN pg_index i ON i.indrelid = a.attrelid AND i.indisprimary \
             WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            &[&oid],
        )
        .await
        .map_err(pg_err)?
        .iter()
        .map(|r| ColumnInfo {
            name: r.get(0),
            data_type: r.get(1),
            nullable: r.get(2),
            default: r.get(3),
            primary_key: r.get(4),
            comment: r.get(5),
            category: r.get::<_, &str>(6).chars().next().unwrap_or('X'),
        })
        .collect();

    let primary_key = client
        .query(
            "SELECT a.attname FROM pg_index i \
             CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord) \
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
             WHERE i.indrelid = $1 AND i.indisprimary ORDER BY k.ord",
            &[&oid],
        )
        .await
        .map_err(pg_err)?
        .iter()
        .map(|r| r.get(0))
        .collect();

    let indexes = client
        .query(
            "SELECT c.relname, pg_get_indexdef(i.indexrelid), i.indisunique, i.indisprimary \
             FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
             WHERE i.indrelid = $1 ORDER BY NOT i.indisprimary, c.relname",
            &[&oid],
        )
        .await
        .map_err(pg_err)?
        .iter()
        .map(|r| IndexInfo {
            name: r.get(0),
            definition: r.get(1),
            unique: r.get(2),
            primary: r.get(3),
        })
        .collect();

    let constraints = client
        .query(
            "SELECT conname, contype::text, pg_get_constraintdef(oid, true) \
             FROM pg_constraint WHERE conrelid = $1 \
             ORDER BY array_position(ARRAY['p','f','u','c','x'], contype::text), conname",
            &[&oid],
        )
        .await
        .map_err(pg_err)?
        .iter()
        .map(|r| ConstraintInfo {
            name: r.get(0),
            kind: match r.get::<_, &str>(1) {
                "p" => "PRIMARY KEY",
                "f" => "FOREIGN KEY",
                "u" => "UNIQUE",
                "c" => "CHECK",
                "x" => "EXCLUDE",
                "t" => "TRIGGER",
                "n" => "NOT NULL",
                _ => "OTHER",
            }
            .to_string(),
            definition: r.get(2),
        })
        .collect();

    let meta = client
        .query_one(
            "SELECT c.relkind::text, obj_description(c.oid, 'pg_class'), c.reltuples::float8, \
                    CASE WHEN c.relkind IN ('v','m') THEN pg_get_viewdef(c.oid, true) END, \
                    n.nspname, c.relname \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.oid = $1",
            &[&oid],
        )
        .await
        .map_err(pg_err)?;
    let relkind: String = meta.get(0);
    let reltuples: f64 = meta.get(2);
    let view_definition = meta.get::<_, Option<String>>(3).map(|body| {
        let schema: String = meta.get(4);
        let name: String = meta.get(5);
        let kw = if relkind == "m" {
            "MATERIALIZED VIEW"
        } else {
            "VIEW"
        };
        format!(
            "CREATE {kw} {} AS\n{}",
            display_qualified(&schema, &name),
            body.trim_end()
        )
    });

    Ok(RelationInfo {
        columns,
        primary_key,
        indexes,
        constraints,
        view_definition,
        comment: meta.get(1),
        // reltuples is -1 for a table that was never vacuumed or analyzed.
        estimated_rows: (reltuples >= 0.0).then_some(reltuples as u64),
        has_ctid: matches!(relkind.as_str(), "r" | "m"),
    })
}

/// Details of a non-relation object: a few labelled properties and SQL.
#[derive(Debug, Clone, Default)]
pub struct ObjectDetail {
    pub properties: Vec<(String, String)>,
    pub definition: String,
}

pub async fn load_object_detail(
    client: &impl GenericClient,
    obj: &DbObject,
) -> anyhow::Result<ObjectDetail> {
    let oid = obj.oid;
    let q = |sql: &'static str| async move { client.query(sql, &[&oid]).await.map_err(pg_err) };
    let name = obj.qualified_name();
    let mut properties = vec![
        ("Schema".to_string(), obj.schema.clone()),
        ("Kind".to_string(), obj.kind.label().to_string()),
    ];
    let definition = match obj.kind {
        ObjectKind::Function | ObjectKind::Procedure => {
            let rows = q("SELECT pg_get_functiondef($1), l.lanname, \
                          pg_get_function_result($1), p.provolatile::text \
                          FROM pg_proc p JOIN pg_language l ON l.oid = p.prolang WHERE p.oid = $1")
            .await?;
            let r = &rows[0];
            properties.push(("Language".into(), r.get(1)));
            if let Some(result) = r.get::<_, Option<String>>(2) {
                properties.push(("Returns".into(), result));
            }
            properties.push((
                "Volatility".into(),
                match r.get::<_, &str>(3) {
                    "i" => "immutable",
                    "s" => "stable",
                    _ => "volatile",
                }
                .into(),
            ));
            r.get::<_, String>(0).trim_end().to_string()
        }
        ObjectKind::Sequence => {
            let row = client
                .query_one(
                    "SELECT data_type::text, start_value, min_value, max_value, increment_by, \
                            cycle, cache_size, last_value \
                     FROM pg_sequences WHERE schemaname = $1 AND sequencename = $2",
                    &[&obj.schema, &obj.name],
                )
                .await
                .map_err(pg_err)?;
            let data_type: String = row.get(0);
            let start: i64 = row.get(1);
            let min: i64 = row.get(2);
            let max: i64 = row.get(3);
            let inc: i64 = row.get(4);
            let cycle: bool = row.get(5);
            let cache: i64 = row.get(6);
            let last: Option<i64> = row.get(7);
            properties.extend([
                (
                    "Current value".to_string(),
                    last.map(|v| v.to_string())
                        .unwrap_or_else(|| "not yet used".into()),
                ),
                ("Data type".into(), data_type.clone()),
                ("Start".into(), start.to_string()),
                ("Increment".into(), inc.to_string()),
                ("Min".into(), min.to_string()),
                ("Max".into(), max.to_string()),
                ("Cache".into(), cache.to_string()),
                ("Cycle".into(), if cycle { "yes" } else { "no" }.into()),
            ]);
            format!(
                "CREATE SEQUENCE {name}\n    AS {data_type}\n    INCREMENT BY {inc}\n    \
                 MINVALUE {min}\n    MAXVALUE {max}\n    START WITH {start}\n    CACHE {cache}\n    \
                 {}CYCLE;",
                if cycle { "" } else { "NO " }
            )
        }
        ObjectKind::Enum => {
            let labels: Vec<String> = q(
                "SELECT enumlabel::text FROM pg_enum WHERE enumtypid = $1 ORDER BY enumsortorder",
            )
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
            properties.push(("Values".into(), labels.len().to_string()));
            for (i, l) in labels.iter().enumerate() {
                properties.push((format!("  {}", i + 1), l.clone()));
            }
            let body = labels
                .iter()
                .map(|l| format!("    {}", quote_literal(l)))
                .collect::<Vec<_>>()
                .join(",\n");
            format!("CREATE TYPE {name} AS ENUM (\n{body}\n);")
        }
        ObjectKind::Domain => {
            let rows = q(
                "SELECT format_type(typbasetype, typtypmod), typnotnull, typdefault \
                          FROM pg_type WHERE oid = $1",
            )
            .await?;
            let r = &rows[0];
            let base: String = r.get(0);
            let not_null: bool = r.get(1);
            let default: Option<String> = r.get(2);
            properties.push(("Base type".into(), base.clone()));
            properties.push((
                "Not null".into(),
                if not_null { "yes" } else { "no" }.into(),
            ));
            let checks: Vec<(String, String)> = q(
                "SELECT conname::text, pg_get_constraintdef(oid, true) FROM pg_constraint \
                 WHERE contypid = $1 AND contype = 'c' ORDER BY conname",
            )
            .await?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
            let mut def = format!("CREATE DOMAIN {name} AS {base}");
            if let Some(d) = &default {
                properties.push(("Default".into(), d.clone()));
                def.push_str(&format!("\n    DEFAULT {d}"));
            }
            if not_null {
                def.push_str("\n    NOT NULL");
            }
            for (cname, cdef) in &checks {
                def.push_str(&format!("\n    CONSTRAINT {} {cdef}", quote_ident(cname)));
            }
            def.push(';');
            def
        }
        ObjectKind::Composite => {
            let attrs: Vec<(String, String)> = q(
                "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod) FROM pg_attribute a \
                 WHERE a.attrelid = (SELECT typrelid FROM pg_type WHERE oid = $1) \
                   AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            )
            .await?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
            for (n, t) in &attrs {
                properties.push((n.clone(), t.clone()));
            }
            let body = attrs
                .iter()
                .map(|(n, t)| format!("    {} {t}", quote_ident(n)))
                .collect::<Vec<_>>()
                .join(",\n");
            format!("CREATE TYPE {name} AS (\n{body}\n);")
        }
        ObjectKind::Range => {
            let rows =
                q("SELECT format_type(rngsubtype, NULL) FROM pg_range WHERE rngtypid = $1").await?;
            let sub: String = rows[0].get(0);
            properties.push(("Subtype".into(), sub.clone()));
            format!("CREATE TYPE {name} AS RANGE (\n    SUBTYPE = {sub}\n);")
        }
        _ => String::new(),
    };
    if let Some(comment) = client
        .query_one(
            "SELECT obj_description($1, $2)",
            &[
                &oid,
                &match obj.kind {
                    ObjectKind::Function | ObjectKind::Procedure => "pg_proc",
                    ObjectKind::Sequence => "pg_class",
                    _ => "pg_type",
                },
            ],
        )
        .await
        .map_err(pg_err)?
        .get::<_, Option<String>>(0)
    {
        properties.push(("Comment".into(), comment));
    }
    Ok(ObjectDetail {
        properties,
        definition,
    })
}
