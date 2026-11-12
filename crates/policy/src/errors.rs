//! Error shaping: make engine errors read like PostgreSQL's.
//!
//! Works on the SQLSTATE, the engine's message and the statement text only. M1 covers the
//! errors the T1 scenarios check; DSQL's own error texts (OC000, OC001, 54000, ...) are M2.

/// An error after shaping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapedError {
    pub message: String,
    /// 1-based character position in the statement text (`P` field), if known.
    pub position: Option<usize>,
    /// Schema name (`s` field), for errors that name a table.
    pub schema: Option<String>,
}

/// Shapes an engine error. `has_table` tells whether the error carries a table name.
pub fn shape_error(sqlstate: &str, message: &str, has_table: bool, sql: &str) -> ShapedError {
    let mut shaped = ShapedError {
        message: message.to_owned(),
        position: None,
        schema: None,
    };
    match sqlstate {
        "42P01" => {
            if let Some(name) = message.strip_prefix("no such table: ") {
                let name = name.trim();
                shaped.message = format!("relation \"{name}\" does not exist");
                shaped.position = find_word(sql, name.rsplit('.').next().unwrap_or(name));
            }
        }
        "42601" => {
            let message = message
                .strip_prefix("Invalid statement: ")
                .unwrap_or(message);
            shaped.message = message.to_owned();
            if let Some(rest) = message.split_once("at or near \"").map(|(_, r)| r) {
                if let Some((token, _)) = rest.split_once('"') {
                    shaped.position = find_word(sql, token);
                }
            } else if message.ends_with("at end of input") {
                shaped.position = Some(sql.trim_end().chars().count() + 1);
            }
        }
        code if code.starts_with("23") && has_table => {
            shaped.schema = Some("public".to_owned());
        }
        _ => {}
    }
    shaped
}

/// 1-based character position of the first occurrence of `word` in `sql` that is not part
/// of a longer identifier (case-insensitive).
fn find_word(sql: &str, word: &str) -> Option<usize> {
    if word.is_empty() {
        return None;
    }
    let lower_sql = sql.to_lowercase();
    let lower_word = word.to_lowercase();
    if lower_sql.len() != sql.len() {
        return None;
    }
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut from = 0;
    while let Some(i) = lower_sql[from..].find(&lower_word) {
        let start = from + i;
        let end = start + lower_word.len();
        let before_ok = !word.starts_with(is_ident)
            || lower_sql[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !is_ident(c));
        let after_ok = !word.ends_with(is_ident)
            || lower_sql[end..].chars().next().is_none_or(|c| !is_ident(c));
        if before_ok && after_ok {
            return Some(sql[..start].chars().count() + 1);
        }
        from = end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undefined_table() {
        let e = shape_error(
            "42P01",
            "no such table: t1_no_such_table",
            false,
            "SELECT * FROM t1_no_such_table;",
        );
        assert_eq!(e.message, "relation \"t1_no_such_table\" does not exist");
        assert_eq!(e.position, Some(15));
    }

    #[test]
    fn syntax_error() {
        let e = shape_error(
            "42601",
            "Invalid statement: syntax error at or near \"SELEC\"",
            false,
            "SELEC 1;",
        );
        assert_eq!(e.message, "syntax error at or near \"SELEC\"");
        assert_eq!(e.position, Some(1));
        let e = shape_error("42601", "syntax error at end of input", false, "SELECT ");
        assert_eq!(e.position, Some(7));
    }

    #[test]
    fn constraint_errors_name_the_schema() {
        let e = shape_error("23505", "duplicate key", true, "INSERT ...");
        assert_eq!(e.schema.as_deref(), Some("public"));
        assert_eq!(shape_error("23505", "x", false, "").schema, None);
    }

    #[test]
    fn find_word_respects_identifier_boundaries() {
        assert_eq!(find_word("select tt from t", "t"), Some(16));
        assert_eq!(find_word("x", "y"), None);
    }
}
