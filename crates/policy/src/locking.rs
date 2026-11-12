//! Locking reads (M4): `SELECT ... FOR UPDATE` and `FOR KEY SHARE` on a single table.
//!
//! The engine has no row locks, so `FOR UPDATE` is planned as two engine statements: a *touch*
//! (`UPDATE t SET c = c WHERE <same where>`, which enters the rows into the transaction's write
//! set so conflicts are detected exactly as for a write) followed by the plain read (the SELECT
//! without its locking clause). `FOR KEY SHARE` is the plain read only. The policy cannot see
//! the schema, so the column of the touch is chosen by the server (constitution III).

use pg_query::protobuf::{
    node::Node as Inner, ColumnRef, LockClauseStrength, ResTarget, SelectStmt, UpdateStmt,
};
use pg_query::{Node, NodeEnum};

use crate::errors::{locking_read_form, PolicyError};

/// Which locking clause a read carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockStrength {
    Update,
    KeyShare,
}

/// A locking read, split into its parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockingRead {
    pub strength: LockStrength,
    /// The SELECT without its locking clause; this is what the engine runs for the read.
    pub read_sql: String,
    /// The write-set touch; `None` for `FOR KEY SHARE`.
    pub touch: Option<TouchPlan>,
}

/// `UPDATE <relation> [AS alias] SET <column> = <column> [WHERE ...]`, with the column left
/// open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchPlan {
    schema: Option<String>,
    relation: String,
    /// The statement up to and including `SET `.
    prefix: String,
    /// What follows `SET <column> = <column>` (the WHERE clause, or nothing).
    suffix: String,
}

/// A placeholder column name that deparses unquoted; replaced by [`TouchPlan::render`].
const PLACEHOLDER: &str = "micro_dsql_touch_column";

impl TouchPlan {
    /// The table the touch writes (its name, without schema).
    pub fn relation(&self) -> &str {
        &self.relation
    }

    /// The schema the SELECT named, if any.
    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    /// The touch statement for `column` (an identifier; quoted here if needed).
    pub fn render(&self, column: &str) -> String {
        let column = quote_ident(column);
        format!("{}{column} = {column}{}", self.prefix, self.suffix)
    }
}

fn quote_ident(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// Plans a locking read. `None`: the statement has no locking clause the policy handles
/// (including `FOR SHARE` / `FOR NO KEY UPDATE`, which an M2 rule refuses). `Some(Err)`: a
/// locking clause on a form that is not supported.
pub fn plan_locking(node: &NodeEnum) -> Option<Result<LockingRead, PolicyError>> {
    let NodeEnum::SelectStmt(select) = node else {
        return None;
    };
    if select.locking_clause.is_empty() {
        // A locking clause hidden in a subquery or CTE.
        let nested = node.nodes().iter().any(
            |(n, ..)| matches!(n, pg_query::NodeRef::SelectStmt(s) if !s.locking_clause.is_empty()),
        );
        return nested.then(|| Err(locking_read_form()));
    }
    let [clause] = select.locking_clause.as_slice() else {
        return Some(Err(locking_read_form()));
    };
    let Some(NodeEnum::LockingClause(clause)) = clause.node.as_ref() else {
        return Some(Err(locking_read_form()));
    };
    let strength = match LockClauseStrength::try_from(clause.strength) {
        Ok(LockClauseStrength::LcsForupdate) => LockStrength::Update,
        Ok(LockClauseStrength::LcsForkeyshare) => LockStrength::KeyShare,
        // `FOR SHARE`, `FOR NO KEY UPDATE`: refused by the M2 rule.
        _ => return None,
    };
    Some(plan_single_table(select, clause, strength))
}

fn plan_single_table(
    select: &SelectStmt,
    clause: &pg_query::protobuf::LockingClause,
    strength: LockStrength,
) -> Result<LockingRead, PolicyError> {
    let simple = select.op == pg_query::protobuf::SetOperation::SetopNone as i32
        && select.with_clause.is_none()
        && select.group_clause.is_empty()
        && select.having_clause.is_none()
        && select.distinct_clause.is_empty()
        && select.window_clause.is_empty()
        && select.into_clause.is_none();
    let [from] = select.from_clause.as_slice() else {
        return Err(locking_read_form());
    };
    let (true, Some(NodeEnum::RangeVar(table))) = (simple, from.node.as_ref()) else {
        return Err(locking_read_form());
    };
    // At most one `OF`, naming the table (or its alias).
    let visible = table
        .alias
        .as_ref()
        .map_or(table.relname.as_str(), |alias| alias.aliasname.as_str());
    match clause.locked_rels.as_slice() {
        [] => {}
        [rel] => match rel.node.as_ref() {
            Some(NodeEnum::RangeVar(of)) if of.schemaname.is_empty() && of.relname == visible => {}
            _ => return Err(locking_read_form()),
        },
        _ => return Err(locking_read_form()),
    }
    let mut read = select.clone();
    read.locking_clause.clear();
    let read_sql = NodeEnum::SelectStmt(Box::new(read))
        .deparse()
        .map_err(|_| locking_read_form())?;
    let touch = match strength {
        LockStrength::Update => Some(plan_touch(select, table)?),
        LockStrength::KeyShare => None,
    };
    Ok(LockingRead {
        strength,
        read_sql,
        touch,
    })
}

fn plan_touch(
    select: &SelectStmt,
    table: &pg_query::protobuf::RangeVar,
) -> Result<TouchPlan, PolicyError> {
    let column = || {
        Box::new(Node {
            node: Some(Inner::ColumnRef(ColumnRef {
                fields: vec![Node {
                    node: Some(Inner::String(pg_query::protobuf::String {
                        sval: PLACEHOLDER.to_owned(),
                    })),
                }],
                location: 0,
            })),
        })
    };
    let update = UpdateStmt {
        relation: Some(table.clone()),
        target_list: vec![Node {
            node: Some(Inner::ResTarget(Box::new(ResTarget {
                name: PLACEHOLDER.to_owned(),
                indirection: Vec::new(),
                val: Some(column()),
                location: 0,
            }))),
        }],
        where_clause: select.where_clause.clone(),
        from_clause: Vec::new(),
        returning_list: Vec::new(),
        with_clause: None,
    };
    let text = NodeEnum::UpdateStmt(Box::new(update))
        .deparse()
        .map_err(|_| locking_read_form())?;
    let assignment = format!("{PLACEHOLDER} = {PLACEHOLDER}");
    let at = text.find(&assignment).ok_or_else(locking_read_form)?;
    Ok(TouchPlan {
        schema: (!table.schemaname.is_empty()).then(|| table.schemaname.clone()),
        relation: table.relname.clone(),
        prefix: text[..at].to_owned(),
        suffix: text[at + assignment.len()..].to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(sql: &str) -> Option<Result<LockingRead, PolicyError>> {
        let parsed = pg_query::parse(sql).unwrap();
        let node = parsed.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .as_ref()
            .unwrap()
            .clone();
        plan_locking(&node)
    }

    fn accepted(sql: &str) -> LockingRead {
        plan(sql)
            .unwrap_or_else(|| panic!("{sql}: no locking read"))
            .unwrap_or_else(|e| panic!("{sql}: refused: {e}"))
    }

    fn refused(sql: &str) {
        let error = plan(sql)
            .unwrap_or_else(|| panic!("{sql}: not planned"))
            .expect_err(sql);
        assert_eq!(error.rule, "locking_read_form", "{sql}");
        assert_eq!(error.sqlstate, "0A000", "{sql}");
    }

    #[test]
    fn for_update_is_a_touch_and_a_plain_read() {
        let read = accepted("SELECT s FROM t WHERE id = 1 FOR UPDATE");
        assert_eq!(read.strength, LockStrength::Update);
        assert_eq!(read.read_sql, "SELECT s FROM t WHERE id = 1");
        let touch = read.touch.expect("a touch");
        assert_eq!(touch.render("v"), "UPDATE t SET v = v WHERE id = 1");
        assert_eq!((touch.relation(), touch.schema()), ("t", None));
    }

    #[test]
    fn of_alias_nowait_and_skip_locked_are_accepted() {
        for sql in [
            "SELECT s FROM t WHERE id = 1 FOR UPDATE OF t",
            "SELECT s FROM t AS x WHERE id = 1 FOR UPDATE OF x",
            "SELECT s FROM t WHERE id = 1 FOR UPDATE NOWAIT",
            "SELECT s FROM t WHERE id = 1 FOR UPDATE SKIP LOCKED",
            "SELECT s FROM t ORDER BY id LIMIT 1 FOR UPDATE",
            "SELECT s FROM t FOR UPDATE",
        ] {
            let read = accepted(sql);
            assert!(!read.read_sql.contains("FOR"), "{sql}: {}", read.read_sql);
            assert!(!read.read_sql.contains("NOWAIT"), "{sql}");
            assert!(read.touch.is_some(), "{sql}");
        }
        let read = accepted("SELECT s FROM t AS x WHERE x.id = 1 FOR UPDATE OF x");
        assert_eq!(
            read.touch.unwrap().render("v"),
            "UPDATE t x SET v = v WHERE x.id = 1"
        );
        let read = accepted("SELECT s FROM t FOR UPDATE");
        assert_eq!(read.touch.unwrap().render("v"), "UPDATE t SET v = v");
        let read = accepted("SELECT s FROM s1.t WHERE id = $1 FOR UPDATE");
        let touch = read.touch.unwrap();
        assert_eq!(touch.render("v"), "UPDATE s1.t SET v = v WHERE id = $1");
        assert_eq!(touch.schema(), Some("s1"));
    }

    #[test]
    fn the_touch_quotes_the_column() {
        let touch = accepted("SELECT * FROM t WHERE id = 1 FOR UPDATE")
            .touch
            .unwrap();
        assert_eq!(
            touch.render("Mixed"),
            "UPDATE t SET \"Mixed\" = \"Mixed\" WHERE id = 1"
        );
    }

    #[test]
    fn for_key_share_is_a_plain_read() {
        let read = accepted("SELECT s FROM t WHERE id = 1 FOR KEY SHARE");
        assert_eq!(read.strength, LockStrength::KeyShare);
        assert_eq!(read.touch, None);
        assert_eq!(read.read_sql, "SELECT s FROM t WHERE id = 1");
    }

    #[test]
    fn unsupported_forms_are_refused() {
        for sql in [
            "SELECT * FROM a JOIN b ON a.id = b.id FOR UPDATE",
            "SELECT * FROM a, b FOR UPDATE",
            "SELECT * FROM (SELECT * FROM a) s FOR UPDATE",
            "SELECT * FROM (SELECT * FROM a FOR UPDATE) s",
            "WITH c AS (SELECT * FROM a FOR UPDATE) SELECT * FROM c",
            "WITH c AS (SELECT 1) SELECT * FROM a FOR UPDATE",
            "SELECT * FROM a UNION SELECT * FROM b FOR UPDATE",
            "SELECT * FROM a FOR UPDATE OF a, b",
            "SELECT * FROM a FOR UPDATE OF b",
            "SELECT v, count(*) FROM a GROUP BY v FOR UPDATE",
            "SELECT DISTINCT v FROM a FOR UPDATE",
            "SELECT * FROM a FOR UPDATE FOR KEY SHARE",
        ] {
            refused(sql);
        }
    }

    #[test]
    fn other_strengths_and_plain_selects_are_not_planned() {
        assert!(plan("SELECT * FROM t FOR SHARE").is_none());
        assert!(plan("SELECT * FROM t FOR NO KEY UPDATE").is_none());
        assert!(plan("SELECT * FROM t").is_none());
        assert!(plan("SELECT 'FOR UPDATE' FROM t").is_none());
        assert!(plan("UPDATE t SET v = 1").is_none());
    }
}
