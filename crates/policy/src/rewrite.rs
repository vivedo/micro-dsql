//! Text rewrites: the statement the engine runs can differ from the one the client sent.
//!
//! Two stages (contracts/policy-api.md §2): [`pre_parse`] works on the raw text before
//! libpg_query sees it (identity for now; M3 strips `ASYNC` here), and [`ast_rewrites`] finds
//! [`TextEdit`]s from the AST (the [`REWRITES`] registry). Error positions are always computed
//! against the client's original text.

use std::borrow::Cow;

use pg_query::NodeEnum;

/// Inserts `insert` at byte offset `at` of the original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub at: usize,
    pub insert: String,
}

/// An AST rewrite: looks at one parsed statement and its text, returns edits.
pub type Rewrite = fn(&NodeEnum, &str) -> Vec<TextEdit>;

/// The registered AST rewrites, by id.
pub const REWRITES: &[(&str, Rewrite)] = &[];

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

    #[test]
    fn no_rewrites_registered_yet() {
        let parsed = pg_query::parse("CREATE TABLE t (n numeric)").unwrap();
        let node = parsed.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .as_ref()
            .unwrap();
        let _ = ast_rewrites(node, "CREATE TABLE t (n numeric)");
    }
}
