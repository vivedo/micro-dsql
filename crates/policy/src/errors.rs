//! Error shaping: make engine errors read like PostgreSQL's.
//!
//! The single error-shaping module: every DSQL error text lives here (OC000, OC001, the
//! transaction limits) next to the PostgreSQL-style shaping of ordinary engine errors.
//!
//! Works on SQLSTATEs, messages and statement text only; the server tells which kind of
//! engine failure happened through [`EngineFailure`].

/// An error raised by micro-dsql itself, not by the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyError {
    /// Rule id from the rule catalogue (logged at debug level).
    pub rule: &'static str,
    pub sqlstate: &'static str,
    pub message: String,
    pub detail: Option<String>,
}

impl PolicyError {
    pub fn new(rule: &'static str, sqlstate: &'static str, message: impl Into<String>) -> Self {
        Self {
            rule,
            sqlstate,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.sqlstate, self.message)
    }
}

impl std::error::Error for PolicyError {}

/// `40001`: a write conflicted with another transaction.
pub fn oc000() -> PolicyError {
    PolicyError::new(
        "oc000",
        "40001",
        "change conflicts with another transaction (OC000)",
    )
}

/// `40001`: the schema was changed by another transaction.
pub fn oc001() -> PolicyError {
    PolicyError::new(
        "oc001",
        "40001",
        "schema has been updated by another transaction (OC001)",
    )
}

/// `54000`: more rows changed in one transaction than DSQL allows.
pub fn row_limit() -> PolicyError {
    PolicyError::new("txn_row_limit", "54000", "transaction row limit exceeded")
}

/// `54000`: the transaction ran longer than the age limit (`secs` seconds).
pub fn age_limit(secs: u64) -> PolicyError {
    PolicyError::new(
        "txn_age_limit",
        "54000",
        format!("transaction age limit of {secs}s exceeded"),
    )
}

/// `0A000`: a second DDL statement in one transaction.
pub fn txn_two_ddl() -> PolicyError {
    PolicyError::new(
        "txn_two_ddl",
        "0A000",
        "multiple ddl statements not supported in a transaction",
    )
}

/// `0A000`: DDL and DML in one transaction.
pub fn txn_ddl_dml() -> PolicyError {
    PolicyError::new(
        "txn_ddl_dml",
        "0A000",
        "ddl and dml are not supported in the same transaction",
    )
}

/// `0A000`: `SAVEPOINT`.
pub fn savepoint() -> PolicyError {
    PolicyError::new(
        "savepoint",
        "0A000",
        "unsupported transaction statement: SAVEPOINT",
    )
}

/// `0A000`: `RELEASE [SAVEPOINT]`.
pub fn release_savepoint() -> PolicyError {
    PolicyError::new(
        "release_savepoint",
        "0A000",
        "unsupported transaction statement: RELEASE",
    )
}

/// `0A000`: `ROLLBACK TO [SAVEPOINT]`.
pub fn rollback_to_savepoint() -> PolicyError {
    PolicyError::new(
        "rollback_to_savepoint",
        "0A000",
        "unsupported transaction statement: ROLLBACK TO",
    )
}

/// What kind of engine failure happened; the only engine fact error shaping needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineFailure {
    /// Write-write conflict, conflicting commit, stale snapshot, aborted commit dependency.
    DataConflict,
    /// The schema changed under the transaction.
    SchemaConflict,
    /// Database busy or table locked (engine SQLSTATE `55P03`).
    LockBusy,
    Other,
}

/// Shapes an engine failure: conflicts and lock contention become DSQL's `40001` errors
/// (`55P03` is an interim mapping until autocommit runs as a transaction, M4); everything
/// else keeps the PostgreSQL-style shaping.
pub fn shape_engine_error(
    failure: EngineFailure,
    sqlstate: &str,
    message: &str,
    detail: Option<&str>,
    has_table: bool,
    original_sql: &str,
) -> ShapedError {
    let policy = match failure {
        EngineFailure::DataConflict | EngineFailure::LockBusy => oc000(),
        EngineFailure::SchemaConflict => oc001(),
        EngineFailure::Other => {
            let mut shaped = shape_error(sqlstate, message, has_table, original_sql);
            shaped.detail = detail.map(str::to_owned);
            return shaped;
        }
    };
    ShapedError {
        sqlstate: policy.sqlstate.to_owned(),
        message: policy.message,
        detail: None,
        position: None,
        schema: None,
    }
}

/// An error after shaping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapedError {
    pub sqlstate: String,
    pub message: String,
    pub detail: Option<String>,
    /// 1-based character position in the statement text (`P` field), if known.
    pub position: Option<usize>,
    /// Schema name (`s` field), for errors that name a table.
    pub schema: Option<String>,
}

/// Shapes an engine error. `has_table` tells whether the error carries a table name.
pub fn shape_error(sqlstate: &str, message: &str, has_table: bool, sql: &str) -> ShapedError {
    let mut shaped = ShapedError {
        sqlstate: sqlstate.to_owned(),
        message: message.to_owned(),
        detail: None,
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
        "3B001" => {
            if let Some(name) = message.strip_prefix("no such savepoint: ") {
                shaped.message = format!("savepoint \"{}\" does not exist", name.trim());
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

// --- M3: DDL rules (contracts/rule-catalogue-m3.md) ---------------------------------------

/// `0A000`: `CREATE INDEX CONCURRENTLY`.
pub fn index_concurrently() -> PolicyError {
    PolicyError::new(
        "index_concurrently",
        "0A000",
        "CONCURRENTLY not supported for CREATE INDEX",
    )
}

/// `0A000`: `CREATE INDEX` without `ASYNC`.
pub fn index_sync() -> PolicyError {
    PolicyError::new(
        "index_sync",
        "0A000",
        "unsupported mode. please use CREATE INDEX ASYNC.",
    )
}

/// `0A000`: an access method other than `btree`.
pub fn index_method() -> PolicyError {
    PolicyError::new(
        "index_method",
        "0A000",
        "USING not supported for CREATE INDEX",
    )
}

/// `54011`: more than eight key columns.
pub fn index_key_columns() -> PolicyError {
    PolicyError::new(
        "index_key_columns",
        "54011",
        "more than 8 column keys in an index are not supported",
    )
}

/// `42809`: `sys.wait_for_job` called from a query.
pub fn wait_for_job_select() -> PolicyError {
    PolicyError::new(
        "wait_for_job_select",
        "42809",
        "sys.wait_for_job(text) is a procedure",
    )
}

/// `0A000`: a subquery in the argument of `CALL sys.wait_for_job`.
pub fn wait_for_job_subquery() -> PolicyError {
    PolicyError::new(
        "wait_for_job_subquery",
        "0A000",
        "cannot use subquery in CALL argument",
    )
}

/// `22P02`: a job id that is not a job id.
pub fn wait_for_job_id() -> PolicyError {
    PolicyError::new("wait_for_job_id", "22P02", "Unable to convert text to UUID")
}

/// `22023`: a well-formed job id that `sys.jobs` does not have.
pub fn wait_for_job_unknown(id: &str) -> PolicyError {
    PolicyError::new(
        "wait_for_job_unknown",
        "22023",
        format!("job {id} not found"),
    )
}

/// `42501`: DML on a `sys` relation.
pub fn sys_write(relation: &str) -> PolicyError {
    PolicyError::new(
        "sys_write",
        "42501",
        format!("permission denied for table {relation}"),
    )
}

/// `42501`: DDL on the `sys` schema.
pub fn sys_ddl() -> PolicyError {
    PolicyError::new("sys_ddl", "42501", "permission denied for schema sys")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_have_the_dsql_texts() {
        let e = oc000();
        assert_eq!(
            (e.rule, e.sqlstate, e.message.as_str(), e.detail),
            (
                "oc000",
                "40001",
                "change conflicts with another transaction (OC000)",
                None
            )
        );
        let e = oc001();
        assert_eq!(e.sqlstate, "40001");
        assert_eq!(
            e.message,
            "schema has been updated by another transaction (OC001)"
        );
        let e = row_limit();
        assert_eq!(
            (e.sqlstate, e.message.as_str()),
            ("54000", "transaction row limit exceeded")
        );
        let e = age_limit(300);
        assert_eq!(
            (e.sqlstate, e.message.as_str()),
            ("54000", "transaction age limit of 300s exceeded")
        );
        assert_eq!(
            txn_two_ddl().message,
            "multiple ddl statements not supported in a transaction"
        );
        assert_eq!(
            txn_ddl_dml().message,
            "ddl and dml are not supported in the same transaction"
        );
        assert_eq!(
            savepoint().message,
            "unsupported transaction statement: SAVEPOINT"
        );
        assert_eq!(
            release_savepoint().message,
            "unsupported transaction statement: RELEASE"
        );
        assert_eq!(
            rollback_to_savepoint().message,
            "unsupported transaction statement: ROLLBACK TO"
        );
        assert_eq!(age_limit(1).message, "transaction age limit of 1s exceeded");
        assert_eq!(
            PolicyError::new("r", "0A000", "m").with_detail("d").detail,
            Some("d".to_owned())
        );
    }

    #[test]
    fn engine_conflicts_become_dsql_errors() {
        for failure in [EngineFailure::DataConflict, EngineFailure::LockBusy] {
            let e = shape_engine_error(failure, "55P03", "Database is busy", None, false, "x");
            assert_eq!(e.sqlstate, "40001");
            assert_eq!(
                e.message,
                "change conflicts with another transaction (OC000)"
            );
            assert_eq!(e.position, None);
        }
        let e = shape_engine_error(
            EngineFailure::SchemaConflict,
            "40001",
            "Database schema conflict",
            None,
            false,
            "x",
        );
        assert_eq!(e.sqlstate, "40001");
        assert_eq!(
            e.message,
            "schema has been updated by another transaction (OC001)"
        );
    }

    #[test]
    fn other_failures_keep_postgres_shaping() {
        let e = shape_engine_error(
            EngineFailure::Other,
            "42P01",
            "no such table: nope",
            None,
            false,
            "SELECT * FROM nope",
        );
        assert_eq!(e.sqlstate, "42P01");
        assert_eq!(e.message, "relation \"nope\" does not exist");
        assert_eq!(e.position, Some(15));
        let e = shape_engine_error(
            EngineFailure::Other,
            "42601",
            "Invalid statement: syntax error at or near \"SELEC\"",
            None,
            false,
            "SELEC 1",
        );
        assert_eq!(e.message, "syntax error at or near \"SELEC\"");
        assert_eq!(e.position, Some(1));
        let e = shape_engine_error(
            EngineFailure::Other,
            "23505",
            "dup",
            Some("Key (id)=(1) already exists."),
            true,
            "INSERT",
        );
        assert_eq!(e.schema.as_deref(), Some("public"));
        assert_eq!(e.detail.as_deref(), Some("Key (id)=(1) already exists."));
    }

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
    fn missing_savepoint_reads_like_postgres() {
        let e = shape_error("3B001", "no such savepoint: sp1", false, "ROLLBACK TO sp1");
        assert_eq!(e.message, "savepoint \"sp1\" does not exist");
        // Other 3B001 texts are untouched.
        assert_eq!(shape_error("3B001", "other", false, "x").message, "other");
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
