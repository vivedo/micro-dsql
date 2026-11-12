//! Text rewrites: the statement the engine runs can differ from the one the client sent.
//!
//! Two stages (contracts/policy-api.md §2): [`pre_parse`] works on the raw text before
//! libpg_query sees it (identity for now; M3 strips `ASYNC` here), and [`ast_rewrites`] finds
//! [`TextEdit`]s from the AST (the [`REWRITES`] registry). Error positions are always computed
//! against the client's original text.

use std::borrow::Cow;

use pg_query::NodeEnum;

use crate::rules::new_columns;

/// Inserts `insert` at byte offset `at` of the original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub at: usize,
    pub insert: String,
}

/// An AST rewrite: looks at one parsed statement and its text, returns edits.
pub type Rewrite = fn(&NodeEnum, &str) -> Vec<TextEdit>;

/// The registered AST rewrites, by id.
pub const REWRITES: &[(&str, Rewrite)] = &[("bare_numeric", bare_numeric)];

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
                insert: "(18,6)".to_owned(),
            })
        })
        .collect()
}

/// Stage 1: raw text before parsing. Identity today.
pub fn pre_parse(sql: &str) -> Cow<'_, str> {
    Cow::Borrowed(sql)
}

/// Applies non-overlapping inserts; offsets are bytes of the original text.
pub fn apply_edits<'a>(sql: &'a str, edits: &[TextEdit]) -> Cow<'a, str> {
    if edits.is_empty() {
        return Cow::Borrowed(sql);
    }
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| e.at);
    let mut out = String::with_capacity(sql.len() + 8 * sorted.len());
    let mut from = 0;
    for edit in sorted {
        let at = edit.at.max(from).min(sql.len());
        out.push_str(&sql[from..at]);
        out.push_str(&edit.insert);
        from = at;
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

    #[test]
    fn pre_parse_is_identity() {
        let out = pre_parse("SELECT 1");
        assert_eq!(out, "SELECT 1");
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn apply_edits_inserts_at_original_offsets() {
        let sql = "abcdef";
        let edits = [
            TextEdit {
                at: 4,
                insert: "[2]".into(),
            },
            TextEdit {
                at: 1,
                insert: "[1]".into(),
            },
        ];
        assert_eq!(apply_edits(sql, &edits), "a[1]bcd[2]ef");
        assert_eq!(apply_edits(sql, &[]), "abcdef");
        assert!(matches!(apply_edits(sql, &[]), Cow::Borrowed(_)));
        let end = [TextEdit {
            at: 6,
            insert: "!".into(),
        }];
        assert_eq!(apply_edits(sql, &end), "abcdef!");
    }

    #[test]
    fn apply_edits_handles_multibyte_text() {
        let sql = "é1";
        let edits = [TextEdit {
            at: 2,
            insert: "-".into(),
        }];
        assert_eq!(apply_edits(sql, &edits), "é-1");
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
