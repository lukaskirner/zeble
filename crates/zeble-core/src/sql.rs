//! SQL text helpers: identifier quoting and the table page query.

/// Quote an identifier with double quotes, doubling embedded quotes.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Quote a string literal, doubling embedded single quotes.
pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// `"schema"."name"`.
pub fn qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(name))
}

/// Reserved words that always need quoting (PostgreSQL's fully reserved list).
const RESERVED: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "both",
    "case",
    "cast",
    "check",
    "collate",
    "column",
    "constraint",
    "create",
    "current_catalog",
    "current_date",
    "current_role",
    "current_time",
    "current_timestamp",
    "current_user",
    "default",
    "deferrable",
    "desc",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "false",
    "fetch",
    "for",
    "foreign",
    "from",
    "grant",
    "group",
    "having",
    "in",
    "initially",
    "intersect",
    "into",
    "lateral",
    "leading",
    "limit",
    "localtime",
    "localtimestamp",
    "not",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "placing",
    "primary",
    "references",
    "returning",
    "select",
    "session_user",
    "some",
    "symmetric",
    "system_user",
    "table",
    "then",
    "to",
    "trailing",
    "true",
    "union",
    "unique",
    "user",
    "using",
    "variadic",
    "when",
    "where",
    "window",
    "with",
];

/// Quote an identifier only when it would not round-trip bare.
pub fn quote_ident_min(name: &str) -> String {
    let mut chars = name.chars();
    let plain = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '$')
        && !RESERVED.contains(&name);
    if plain {
        name.to_string()
    } else {
        quote_ident(name)
    }
}

/// `schema.name` for display, quoting only where needed.
pub fn display_qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", quote_ident_min(schema), quote_ident_min(name))
}

/// The direction of a header-click sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

/// What keeps page boundaries stable when the user has not sorted (and
/// breaks ties when they have).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StableOrder {
    PrimaryKey(Vec<String>),
    Ctid,
    None,
}

/// One page of a table or view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageQuery {
    pub schema: String,
    pub name: String,
    pub sort: Option<(String, SortDir)>,
    pub stable: StableOrder,
    pub page: usize,
    pub page_size: usize,
}

impl PageQuery {
    pub fn to_sql(&self) -> String {
        let mut order: Vec<String> = Vec::new();
        if let Some((col, dir)) = &self.sort {
            order.push(format!(
                "{} {}",
                quote_ident(col),
                match dir {
                    SortDir::Asc => "ASC",
                    SortDir::Desc => "DESC",
                }
            ));
        }
        match &self.stable {
            StableOrder::PrimaryKey(cols) => {
                for c in cols {
                    if self.sort.as_ref().is_none_or(|(s, _)| s != c) {
                        order.push(quote_ident(c));
                    }
                }
            }
            StableOrder::Ctid => order.push("ctid".into()),
            StableOrder::None => {}
        }
        let mut sql = format!("SELECT * FROM {}", qualified(&self.schema, &self.name));
        if !order.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&order.join(", "));
        }
        sql.push_str(&format!(
            " LIMIT {} OFFSET {}",
            self.page_size,
            self.page * self.page_size
        ));
        sql
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_literal("it's"), "'it''s'");
        assert_eq!(qualified("public", "Users"), "\"public\".\"Users\"");
        assert_eq!(display_qualified("public", "Users"), "public.\"Users\"");
        assert_eq!(
            display_qualified("my schema", "order"),
            "\"my schema\".\"order\""
        );
        assert_eq!(display_qualified("shop", "order_items"), "shop.order_items");
    }

    fn page(sort: Option<(&str, SortDir)>, stable: StableOrder, page: usize) -> String {
        PageQuery {
            schema: "s".into(),
            name: "t".into(),
            sort: sort.map(|(c, d)| (c.to_string(), d)),
            stable,
            page,
            page_size: 500,
        }
        .to_sql()
    }

    #[test]
    fn page_sql() {
        assert_eq!(
            page(None, StableOrder::PrimaryKey(vec!["id".into()]), 0),
            "SELECT * FROM \"s\".\"t\" ORDER BY \"id\" LIMIT 500 OFFSET 0"
        );
        assert_eq!(
            page(Some(("name", SortDir::Desc)), StableOrder::Ctid, 2),
            "SELECT * FROM \"s\".\"t\" ORDER BY \"name\" DESC, ctid LIMIT 500 OFFSET 1000"
        );
        assert_eq!(
            page(
                Some(("id", SortDir::Desc)),
                StableOrder::PrimaryKey(vec!["id".into(), "b".into()]),
                0
            ),
            "SELECT * FROM \"s\".\"t\" ORDER BY \"id\" DESC, \"b\" LIMIT 500 OFFSET 0"
        );
        assert_eq!(
            page(None, StableOrder::None, 1),
            "SELECT * FROM \"s\".\"t\" LIMIT 500 OFFSET 500"
        );
    }
}
