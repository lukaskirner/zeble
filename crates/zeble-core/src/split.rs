//! Splitting a SQL script into statements, so each runs (and is timed) on
//! its own, the way psql sends them.

/// Statements in `sql` as `(byte offset, text)`, without the trailing `;`.
///
/// Semicolons inside quotes, quoted identifiers, dollar-quoted bodies and
/// comments do not split. Chunks holding only whitespace and comments are
/// dropped. SQL-standard `BEGIN ATOMIC ... END` bodies are not recognised.
pub fn split_statements(sql: &str) -> Vec<(usize, &str)> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let mut has_code = false;
    while i < b.len() {
        let c = b[i];
        match c {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 1;
                i += 2;
                while i < b.len() && depth > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            b'\'' | b'"' => {
                has_code = true;
                // E'...' strings treat backslash as an escape.
                let escapes = c == b'\''
                    && i > 0
                    && matches!(b[i - 1], b'e' | b'E')
                    && (i < 2 || !(b[i - 2].is_ascii_alphanumeric() || b[i - 2] == b'_'));
                i += 1;
                while i < b.len() {
                    if escapes && b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == c {
                        if b.get(i + 1) == Some(&c) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
                continue;
            }
            b'$' => {
                // $tag$ ... $tag$ (tag may be empty); not $1 parameters.
                let prev_ident = i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
                let mut j = i + 1;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                let tag_ok = j < b.len()
                    && b[j] == b'$'
                    && !prev_ident
                    && !b.get(i + 1).is_some_and(|d| d.is_ascii_digit());
                if tag_ok {
                    has_code = true;
                    let tag = &sql[i..=j];
                    match sql[j + 1..].find(tag) {
                        Some(end) => i = j + 1 + end + tag.len(),
                        None => i = b.len(),
                    }
                    continue;
                }
                has_code = true;
            }
            b';' => {
                if has_code {
                    out.push((start, &sql[start..i]));
                }
                start = i + 1;
                has_code = false;
            }
            c if !c.is_ascii_whitespace() => has_code = true,
            _ => {}
        }
        i += 1;
    }
    if has_code {
        out.push((start, &sql[start..]));
    }
    out
}

/// The byte range of the statement the cursor (a byte offset) belongs to,
/// trimmed of surrounding whitespace and without its `;`.
///
/// A cursor inside a statement or right after its `;` belongs to it. A cursor
/// in whitespace or comments between statements belongs to the nearest
/// statement above, or to the first statement when there is none above.
pub fn statement_at(sql: &str, cursor: usize) -> Option<std::ops::Range<usize>> {
    let ranges: Vec<std::ops::Range<usize>> = split_statements(sql)
        .into_iter()
        .map(|(offset, text)| {
            let start = offset + leading_trivia(text);
            let end = offset + text.trim_end().len();
            start..end
        })
        .collect();
    let first = ranges.first()?.clone();
    // The last statement whose start is at or before the cursor, so a cursor
    // in the gap after a statement still belongs to it.
    Some(
        ranges
            .iter()
            .rev()
            .find(|r| r.start <= cursor)
            .cloned()
            .unwrap_or(first),
    )
}

/// Length of the whitespace and comments at the start of `text`.
fn leading_trivia(text: &str) -> usize {
    let b = text.as_bytes();
    let mut i = 0;
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b[i..].starts_with(b"--") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            let mut depth = 0;
            while i < b.len() {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else {
            return i;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{split_statements, statement_at};

    fn current(sql_with_cursor: &str) -> Option<String> {
        let cursor = sql_with_cursor.find('|').unwrap();
        let sql = sql_with_cursor.replace('|', "");
        statement_at(&sql, cursor).map(|r| sql[r].to_string())
    }

    #[test]
    fn statement_under_cursor() {
        assert_eq!(
            current("SELECT 1;\nSEL|ECT 2;\nSELECT 3").unwrap(),
            "SELECT 2"
        );
        assert_eq!(current("SELECT 1;|\nSELECT 2").unwrap(), "SELECT 1");
        assert_eq!(current("SELECT 1;\n\n|\nSELECT 2").unwrap(), "SELECT 1");
        assert_eq!(
            current("SELECT 1; -- note |\nSELECT 2").unwrap(),
            "SELECT 1"
        );
        assert_eq!(current("\n|\n  SELECT 1;\nSELECT 2").unwrap(), "SELECT 1");
        assert_eq!(current("SELECT 1;\nSELECT 2|").unwrap(), "SELECT 2");
        assert_eq!(current("SELECT 1;\n|SELECT 2").unwrap(), "SELECT 2");
        assert_eq!(
            current("SELECT $$a;|b$$;\nSELECT 2").unwrap(),
            "SELECT $$a;b$$"
        );
        assert_eq!(
            current("SELECT\n  1\n\n  FROM t|;").unwrap(),
            "SELECT\n  1\n\n  FROM t"
        );
        assert!(current("  |-- only a comment\n").is_none());
        assert!(current("|").is_none());
    }

    fn texts(sql: &str) -> Vec<String> {
        split_statements(sql)
            .into_iter()
            .map(|(_, s)| s.trim().to_string())
            .collect()
    }

    #[test]
    fn splits_simple() {
        assert_eq!(texts("SELECT 1; SELECT 2;\n\n"), ["SELECT 1", "SELECT 2"]);
        assert_eq!(texts("SELECT 1"), ["SELECT 1"]);
        assert!(texts("  ;; -- nothing\n /* x; */ ").is_empty());
    }

    #[test]
    fn ignores_semicolons_in_quotes_and_comments() {
        assert_eq!(
            texts("SELECT 'a;b', \"c;d\", E'x\\';y' -- ;\n; SELECT /* ; /* ; */ */ 2"),
            [
                "SELECT 'a;b', \"c;d\", E'x\\';y' -- ;",
                "SELECT /* ; /* ; */ */ 2"
            ]
        );
        assert_eq!(
            texts("SELECT 'it''s; fine'; SELECT 3"),
            ["SELECT 'it''s; fine'", "SELECT 3"]
        );
    }

    #[test]
    fn dollar_quotes() {
        let sql = "CREATE FUNCTION f() RETURNS int AS $body$ BEGIN; RETURN 1; END $body$ LANGUAGE plpgsql; SELECT $$a;b$$, $1";
        assert_eq!(
            texts(sql),
            [
                "CREATE FUNCTION f() RETURNS int AS $body$ BEGIN; RETURN 1; END $body$ LANGUAGE plpgsql",
                "SELECT $$a;b$$, $1"
            ]
        );
    }

    #[test]
    fn offsets_point_into_source() {
        let sql = "SELECT 1;\n  SELECT 2";
        let parts = split_statements(sql);
        assert_eq!(parts[1].0, 9);
        assert_eq!(&sql[parts[1].0..], "\n  SELECT 2");
    }
}
