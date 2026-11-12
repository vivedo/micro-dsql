//! Text rewrites: the statement the engine runs can differ from the one the client sent.
//!
//! Two stages (contracts/policy-api.md §2): [`pre_parse`] works on the raw text before
//! libpg_query sees it (identity for now; M3 strips `ASYNC` here), and [`ast_rewrites`] finds
//! [`TextEdit`]s from the AST (the [`REWRITES`] registry). Error positions are always computed
//! against the client's original text.

use std::borrow::Cow;

use pg_query::protobuf::Token;
use pg_query::NodeEnum;

use crate::rules::new_columns;

/// Replaces the `remove` bytes at byte offset `at` of the original text with `insert`
/// (`remove == 0` is a pure insert).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub at: usize,
    pub remove: usize,
    pub insert: String,
}

/// Which statement the DSQL `ASYNC` keyword was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncKind {
    /// `CREATE [UNIQUE] INDEX ASYNC ...`
    Index,
    /// `ALTER TABLE ASYNC ...`
    AlterTable,
}

/// The result of [`pre_parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreParsed<'a> {
    /// The text with the `ASYNC` keyword blanked (same byte length as the input).
    pub text: Cow<'a, str>,
    pub async_kind: Option<AsyncKind>,
}

/// An AST rewrite: looks at one parsed statement and its text, returns edits.
pub type Rewrite = fn(&NodeEnum, &str) -> Vec<TextEdit>;

/// The registered AST rewrites, by id.
pub const REWRITES: &[(&str, Rewrite)] = &[
    ("bare_numeric", bare_numeric),
    ("index_include", index_include),
    ("index_nulls_not_distinct", index_nulls_not_distinct),
];

/// `CREATE UNIQUE INDEX ... NULLS NOT DISTINCT`: the engine does not have it, so the clause is
/// removed (known gap: NULLs stay distinct).
fn index_nulls_not_distinct(node: &NodeEnum, sql: &str) -> Vec<TextEdit> {
    let NodeEnum::IndexStmt(index) = node else {
        return Vec::new();
    };
    if !index.nulls_not_distinct {
        return Vec::new();
    }
    let tokens = scan_positions(sql);
    tokens
        .windows(3)
        .find(|w| {
            matches!(
                (w[0].0, w[1].0, w[2].0),
                (Token::NullsP, Token::Not, Token::Distinct)
            )
        })
        .map(|w| TextEdit {
            at: w[0].1,
            remove: w[2].2 - w[0].1,
            insert: String::new(),
        })
        .into_iter()
        .collect()
}

/// `CREATE INDEX ... INCLUDE (cols)`: the engine has no covering columns, so the clause is
/// removed (known gap: the index is built without them; results are the same).
fn index_include(node: &NodeEnum, sql: &str) -> Vec<TextEdit> {
    let NodeEnum::IndexStmt(index) = node else {
        return Vec::new();
    };
    if index.index_including_params.is_empty() {
        return Vec::new();
    }
    let tokens = scan_positions(sql);
    let mut depth = 0_i32;
    let mut edits = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let (token, start, _) = tokens[i];
        match (token, &sql[tokens[i].1..tokens[i].2]) {
            (_, "(") => depth += 1,
            (_, ")") => depth -= 1,
            (Token::Include, _) if depth == 0 && edits.is_empty() => {
                // From INCLUDE through the matching closing parenthesis.
                let mut inner = 0_i32;
                for &(_, s, e) in &tokens[i + 1..] {
                    match &sql[s..e] {
                        "(" => inner += 1,
                        ")" => {
                            inner -= 1;
                            if inner == 0 {
                                edits.push(TextEdit {
                                    at: start,
                                    remove: e - start,
                                    insert: String::new(),
                                });
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    edits
}

/// `numeric` / `decimal` without precision in `CREATE TABLE` and `ALTER TABLE ... ADD COLUMN`
/// means `numeric(18,6)` on DSQL (FR-040): insert `(18,6)` after the type name token.
fn bare_numeric(node: &NodeEnum, sql: &str) -> Vec<TextEdit> {
    let columns = new_columns(node);
    let bare: Vec<usize> = columns
        .iter()
        .filter_map(|col| col.type_name.as_ref())
        .filter(|ty| {
            ty.typmods.is_empty()
                && ty.array_bounds.is_empty()
                && matches!(
                    ty.names.last().and_then(|n| n.node.as_ref()),
                    Some(NodeEnum::String(s)) if s.sval == "numeric"
                )
        })
        .filter_map(|ty| usize::try_from(ty.location).ok())
        .collect();
    if bare.is_empty() {
        return Vec::new();
    }
    let Ok(scanned) = pg_query::scan(sql) else {
        return Vec::new();
    };
    bare.into_iter()
        .filter_map(|start| {
            let token = scanned
                .tokens
                .iter()
                .find(|t| usize::try_from(t.start).ok() == Some(start))?;
            Some(TextEdit {
                at: usize::try_from(token.end).ok()?,
                remove: 0,
                insert: "(18,6)".to_owned(),
            })
        })
        .collect()
}

/// The tokens of `sql` with their byte ranges (empty when the scanner fails).
fn scan_positions(sql: &str) -> Vec<(Token, usize, usize)> {
    let Ok(scanned) = pg_query::scan(sql) else {
        return Vec::new();
    };
    scanned
        .tokens
        .iter()
        .filter_map(|t| {
            Some((
                Token::try_from(t.token).ok()?,
                usize::try_from(t.start).ok()?,
                usize::try_from(t.end).ok()?,
            ))
        })
        .collect()
}

/// Stage 1: raw text before parsing (research.md D1).
///
/// DSQL's `ASYNC` keyword (`CREATE [UNIQUE] INDEX ASYNC ...`, `ALTER TABLE ASYNC ...`) is not
/// PostgreSQL syntax. It is replaced by five spaces, so every byte offset (error positions,
/// AST locations, later edits) means the same in both texts. Only a keyword token counts:
/// comments, string literals and quoted identifiers are never touched, and `ALTER TABLE async
/// ADD COLUMN ...` (a table called `async`) is left alone.
pub fn pre_parse(sql: &str) -> PreParsed<'_> {
    let unchanged = || PreParsed {
        text: Cow::Borrowed(sql),
        async_kind: None,
    };
    // Only text that mentions the word is scanned.
    if !sql
        .as_bytes()
        .windows(5)
        .any(|w| w.eq_ignore_ascii_case(b"async"))
    {
        return unchanged();
    }
    let tokens = scan_positions(sql);
    let is_async = |i: usize| {
        tokens.get(i).is_some_and(|&(token, start, end)| {
            token == Token::Ident
                && sql
                    .get(start..end)
                    .is_some_and(|text| text.eq_ignore_ascii_case("async"))
        })
    };
    let kind_at = |i: usize| tokens.get(i).map(|t| t.0);
    let mut blanks: Vec<(usize, usize)> = Vec::new();
    let mut found = None;
    for i in 0..tokens.len() {
        let (kind, at) = match kind_at(i) {
            Some(Token::Create) => {
                let mut j = i + 1;
                if kind_at(j) == Some(Token::Unique) {
                    j += 1;
                }
                if kind_at(j) != Some(Token::Index) || !is_async(j + 1) {
                    continue;
                }
                // `CREATE INDEX ASYNC public.i`: the blanked text fails at the dot, like DSQL.
                (AsyncKind::Index, j + 1)
            }
            Some(Token::Alter) => {
                // `ALTER TABLE ASYNC t ...`: a table called `async` is followed by a keyword.
                if kind_at(i + 1) != Some(Token::Table)
                    || !is_async(i + 2)
                    || kind_at(i + 3) != Some(Token::Ident)
                {
                    continue;
                }
                (AsyncKind::AlterTable, i + 2)
            }
            _ => continue,
        };
        blanks.push((tokens[at].1, tokens[at].2));
        found.get_or_insert(kind);
    }
    let Some(async_kind) = found else {
        return unchanged();
    };
    let mut text = sql.to_owned();
    for (start, end) in blanks {
        text.replace_range(start..end, &" ".repeat(end - start));
    }
    PreParsed {
        text: Cow::Owned(text),
        async_kind: Some(async_kind),
    }
}

/// Applies non-overlapping edits; offsets are bytes of the original text. An insert may sit
/// at the start or end of a replaced span.
pub fn apply_edits<'a>(sql: &'a str, edits: &[TextEdit]) -> Cow<'a, str> {
    if edits.is_empty() {
        return Cow::Borrowed(sql);
    }
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| (e.at, e.remove));
    let mut out = String::with_capacity(sql.len() + 8 * sorted.len());
    let mut from = 0;
    for edit in sorted {
        debug_assert!(edit.at >= from, "overlapping edits at byte {}", edit.at);
        let at = edit.at.max(from).min(sql.len());
        out.push_str(&sql[from..at]);
        out.push_str(&edit.insert);
        from = (at + edit.remove).min(sql.len());
    }
    out.push_str(&sql[from..]);
    Cow::Owned(out)
}

/// Stage 2: all edits the registered rewrites want for this statement.
pub fn ast_rewrites(node: &NodeEnum, sql: &str) -> Vec<TextEdit> {
    REWRITES
        .iter()
        .flat_map(|(_, rewrite)| rewrite(node, sql))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blanked(sql: &str) -> (String, Option<AsyncKind>) {
        let pre = pre_parse(sql);
        (pre.text.into_owned(), pre.async_kind)
    }

    #[test]
    fn async_after_create_index_is_blanked() {
        for (sql, kind) in [
            ("CREATE INDEX ASYNC i ON t (c)", AsyncKind::Index),
            ("CREATE UNIQUE INDEX ASYNC i ON t (c)", AsyncKind::Index),
            ("create index async i on t (c)", AsyncKind::Index),
            (
                "CREATE INDEX ASYNC IF NOT EXISTS i ON t (c)",
                AsyncKind::Index,
            ),
            ("CREATE INDEX ASYNC ON t (c)", AsyncKind::Index),
            (
                "ALTER TABLE ASYNC t VALIDATE CONSTRAINT c",
                AsyncKind::AlterTable,
            ),
            (
                "alter table async \"t\" validate constraint c",
                AsyncKind::AlterTable,
            ),
        ] {
            let (text, found) = blanked(sql);
            assert_eq!(found, Some(kind), "{sql}");
            assert_eq!(text.len(), sql.len(), "{sql}");
            assert!(!text.to_ascii_lowercase().contains("async"), "{text}");
            let at = sql.to_ascii_lowercase().find("async").unwrap();
            assert_eq!(&text[at..at + 5], "     ", "{sql}");
            assert_eq!(&text[..at], &sql[..at]);
            assert_eq!(&text[at + 5..], &sql[at + 5..]);
        }
    }

    #[test]
    fn async_elsewhere_is_left_alone() {
        for sql in [
            "SELECT 1",
            "SELECT 'CREATE INDEX ASYNC i ON t (c)'",
            "/* CREATE INDEX ASYNC i ON t (c) */ SELECT 1",
            "-- CREATE INDEX ASYNC i ON t (c)\nSELECT 1",
            "CREATE INDEX \"async\" ON t (c)",
            "CREATE TABLE async (id int)",
            "ALTER TABLE async ADD COLUMN c int",
            "ALTER TABLE \"async\" ADD COLUMN c int",
            "SELECT async FROM t",
            "CREATE INDEX i ON t (async)",
        ] {
            let pre = pre_parse(sql);
            assert_eq!(pre.async_kind, None, "{sql}");
            assert_eq!(pre.text, sql);
            assert!(matches!(pre.text, Cow::Borrowed(_)), "{sql}");
        }
    }

    #[test]
    fn a_qualified_async_index_name_still_fails_to_parse() {
        let (text, kind) = blanked("CREATE INDEX ASYNC public.i ON t (c)");
        assert_eq!(kind, Some(AsyncKind::Index));
        assert!(pg_query::parse(&text).is_err());
        assert!(pg_query::parse("CREATE INDEX ASYNC i ON t (c)").is_err());
        assert!(pg_query::parse(&blanked("CREATE INDEX ASYNC i ON t (c)").0).is_ok());
    }

    #[test]
    fn async_in_a_multi_statement_string() {
        let (text, kind) = blanked("BEGIN; CREATE INDEX ASYNC i ON t (c); COMMIT");
        assert_eq!(kind, Some(AsyncKind::Index));
        assert_eq!(text, "BEGIN; CREATE INDEX       i ON t (c); COMMIT");
    }

    #[test]
    fn apply_edits_inserts_at_original_offsets() {
        let sql = "abcdef";
        let edits = [
            TextEdit {
                at: 4,
                remove: 0,
                insert: "[2]".into(),
            },
            TextEdit {
                at: 1,
                remove: 0,
                insert: "[1]".into(),
            },
        ];
        assert_eq!(apply_edits(sql, &edits), "a[1]bcd[2]ef");
        assert_eq!(apply_edits(sql, &[]), "abcdef");
        assert!(matches!(apply_edits(sql, &[]), Cow::Borrowed(_)));
        let end = [TextEdit {
            at: 6,
            remove: 0,
            insert: "!".into(),
        }];
        assert_eq!(apply_edits(sql, &end), "abcdef!");
    }

    #[test]
    fn apply_edits_handles_multibyte_text() {
        let sql = "é1";
        let edits = [TextEdit {
            at: 2,
            remove: 0,
            insert: "-".into(),
        }];
        assert_eq!(apply_edits(sql, &edits), "é-1");
        let replace = [TextEdit {
            at: 0,
            remove: 2,
            insert: "e".into(),
        }];
        assert_eq!(apply_edits(sql, &replace), "e1");
    }

    #[test]
    fn apply_edits_replaces_spans() {
        let sql = "abcdef";
        let edits = [
            TextEdit {
                at: 3,
                remove: 2,
                insert: "XYZ".into(),
            },
            TextEdit {
                at: 1,
                remove: 1,
                insert: String::new(),
            },
            TextEdit {
                at: 0,
                remove: 0,
                insert: ">".into(),
            },
        ];
        assert_eq!(apply_edits(sql, &edits), ">acXYZf");
        // An insert at the start of a replaced span and one at its end.
        let edits = [
            TextEdit {
                at: 2,
                remove: 2,
                insert: "-".into(),
            },
            TextEdit {
                at: 4,
                remove: 0,
                insert: "+".into(),
            },
        ];
        assert_eq!(apply_edits(sql, &edits), "ab-+ef");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "overlapping")]
    fn apply_edits_rejects_overlapping_edits() {
        let edits = [
            TextEdit {
                at: 1,
                remove: 3,
                insert: "x".into(),
            },
            TextEdit {
                at: 2,
                remove: 1,
                insert: "y".into(),
            },
        ];
        let _ = apply_edits("abcdef", &edits);
    }

    fn rewritten(sql: &str) -> String {
        let parsed = pg_query::parse(sql).unwrap();
        let node = parsed.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .as_ref()
            .unwrap();
        apply_edits(sql, &ast_rewrites(node, sql)).into_owned()
    }

    #[test]
    fn bare_numeric_in_create_table() {
        assert_eq!(
            rewritten("CREATE TABLE t (id int, n numeric, d decimal, e numeric(5), f numeric(10,2), g decimal(4,1))"),
            "CREATE TABLE t (id int, n numeric(18,6), d decimal(18,6), e numeric(5), f numeric(10,2), g decimal(4,1))"
        );
        assert_eq!(
            rewritten("CREATE TABLE t (n numeric NOT NULL DEFAULT 1, m NUMERIC)"),
            "CREATE TABLE t (n numeric(18,6) NOT NULL DEFAULT 1, m NUMERIC(18,6))"
        );
        assert_eq!(
            rewritten("CREATE TABLE t (n numeric)"),
            "CREATE TABLE t (n numeric(18,6))"
        );
    }

    #[test]
    fn bare_numeric_in_add_column() {
        assert_eq!(
            rewritten("ALTER TABLE t ADD COLUMN n numeric"),
            "ALTER TABLE t ADD COLUMN n numeric(18,6)"
        );
        assert_eq!(
            rewritten("ALTER TABLE t ADD COLUMN IF NOT EXISTS n decimal NOT NULL, ADD COLUMN k numeric(3)"),
            "ALTER TABLE t ADD COLUMN IF NOT EXISTS n decimal(18,6) NOT NULL, ADD COLUMN k numeric(3)"
        );
    }

    #[test]
    fn include_columns_are_dropped_from_the_engine_text() {
        assert_eq!(
            rewritten("CREATE INDEX i ON t (c) INCLUDE (d, e)"),
            "CREATE INDEX i ON t (c) "
        );
        assert_eq!(
            rewritten("CREATE UNIQUE INDEX i ON t (c) INCLUDE (d) WHERE c > 0"),
            "CREATE UNIQUE INDEX i ON t (c)  WHERE c > 0"
        );
        assert_eq!(
            rewritten("CREATE INDEX i ON t USING btree ((lower(c)), d) include (\"x y\") "),
            "CREATE INDEX i ON t USING btree ((lower(c)), d)  "
        );
        for sql in [
            "CREATE INDEX i ON t (c)",
            "CREATE INDEX i ON t (c) WHERE c = 'INCLUDE (d)'",
            "CREATE TABLE t (include int)",
            "SELECT 1 /* INCLUDE (d) */",
        ] {
            assert_eq!(rewritten(sql), sql);
        }
    }

    #[test]
    fn nulls_not_distinct_is_dropped_from_the_engine_text() {
        assert_eq!(
            rewritten("CREATE UNIQUE INDEX i ON t (c) NULLS NOT DISTINCT"),
            "CREATE UNIQUE INDEX i ON t (c) "
        );
        assert_eq!(
            rewritten("CREATE UNIQUE INDEX i ON t (c) nulls not distinct WHERE c > 0"),
            "CREATE UNIQUE INDEX i ON t (c)  WHERE c > 0"
        );
        for sql in [
            "CREATE UNIQUE INDEX i ON t (c) NULLS DISTINCT",
            "CREATE INDEX i ON t (c DESC NULLS FIRST)",
            "CREATE INDEX i ON t (c) WHERE c = 'NULLS NOT DISTINCT'",
        ] {
            assert_eq!(rewritten(sql), sql);
        }
    }

    #[test]
    fn only_the_type_name_changes() {
        // Quoted identifiers and comments that say "numeric" are not touched.
        assert_eq!(
            rewritten(
                "CREATE TABLE \"numeric\" (\"numeric\" int, n numeric /* numeric */) -- numeric"
            ),
            "CREATE TABLE \"numeric\" (\"numeric\" int, n numeric(18,6) /* numeric */) -- numeric"
        );
        for sql in [
            "SELECT 1.5::numeric",
            "SELECT CAST(x AS numeric) FROM t",
            "CREATE TABLE t (id int, n numeric(5,2))",
            "ALTER TABLE t ALTER COLUMN n TYPE numeric",
            "INSERT INTO t VALUES (1)",
            "CREATE TABLE t (n int)",
        ] {
            assert_eq!(rewritten(sql), sql);
        }
    }
}
