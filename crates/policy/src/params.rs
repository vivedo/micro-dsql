//! Parameter and result-column hints from the AST.
//!
//! The engine does not report parameter types, and reports no type for many expressions.
//! These hints let the wire server describe statements the way drivers expect (M0 blockers
//! B1 and B3). They are hints, not a type checker: anything not recognised stays unknown.

use pg_query::protobuf::{AExprKind, Node, Token};
use pg_query::{NodeEnum, NodeRef};

/// What is known about a `$n` parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamHint {
    /// Nothing recognised.
    Unknown,
    /// An explicit cast `$n::type`; the type name as written (last name component, lower
    /// case), `[]` appended for arrays.
    Cast(String),
    /// Compared with, assigned to or inserted into a table column.
    Column {
        /// Candidate tables (`schema.table` or `table`), in statement order.
        tables: Vec<String>,
        column: Column,
    },
    /// Used where PostgreSQL wants `bigint` (`LIMIT`, `OFFSET`).
    Int8,
}

/// A column named or positioned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Column {
    Name(String),
    /// Zero-based position, for `INSERT INTO t VALUES (...)` without a column list.
    Position(usize),
}

/// A type known for a result column regardless of the engine's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultHint {
    /// `count(...)` is `bigint`.
    Int8,
    /// Comparisons, `AND`/`OR`/`NOT`, `IS [NOT] NULL`, `IN`, `LIKE`, ... are `boolean`.
    Bool,
}

/// Parameter and result hints for one statement.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatementShape {
    /// One entry per parameter, `$1` first; the length is the highest `$n` used.
    pub params: Vec<ParamHint>,
    /// One entry per result column when the target list could be mapped (no `*`);
    /// empty otherwise.
    pub results: Vec<Option<ResultHint>>,
}

/// Analyses one statement. Text that does not parse gives an empty shape.
pub fn analyze(sql: &str) -> StatementShape {
    let Ok(parsed) = pg_query::parse(sql) else {
        return StatementShape::default();
    };
    let nodes = parsed.protobuf.nodes();

    let tables: Vec<String> = nodes
        .iter()
        .filter_map(|(node, ..)| match node {
            NodeRef::RangeVar(rv) => Some(qualified(&rv.schemaname, &rv.relname)),
            _ => None,
        })
        .fold(Vec::new(), |mut acc, t| {
            if !acc.contains(&t) {
                acc.push(t);
            }
            acc
        });

    // pg_query's node walk skips some clauses (VALUES lists, LIMIT), so parameters are
    // counted from the token stream instead.
    let count = param_count(sql);
    let mut params = vec![ParamHint::Unknown; count];
    let mut set = |n: i32, hint: ParamHint| {
        let Some(slot) = usize::try_from(n)
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| params.get_mut(i))
        else {
            return;
        };
        // A cast is the strongest hint, then a column, then int8.
        let rank = |h: &ParamHint| match h {
            ParamHint::Unknown => 0,
            ParamHint::Int8 => 1,
            ParamHint::Column { .. } => 2,
            ParamHint::Cast(_) => 3,
        };
        if rank(&hint) > rank(slot) {
            *slot = hint;
        }
    };
    let column_hint = |name: String| ParamHint::Column {
        tables: tables.clone(),
        column: Column::Name(name),
    };

    for (node, ..) in &nodes {
        match node {
            NodeRef::TypeCast(tc) => {
                if let (Some(n), Some(type_name)) = (param_number(tc.arg.as_deref()), &tc.type_name)
                {
                    if let Some(name) = type_name.names.last().and_then(string_value) {
                        let mut name = name.to_ascii_lowercase();
                        if !type_name.array_bounds.is_empty() {
                            name.push_str("[]");
                        }
                        set(n, ParamHint::Cast(name));
                    }
                }
            }
            NodeRef::AExpr(e) => {
                let l = e.lexpr.as_deref();
                let r = e.rexpr.as_deref();
                if let (Some(col), Some(n)) = (column_name(l), param_number(r)) {
                    set(n, column_hint(col));
                }
                if let (Some(n), Some(col)) = (param_number(l), column_name(r)) {
                    set(n, column_hint(col));
                }
                // col IN ($1, $2), col BETWEEN $1 AND $2
                if let (Some(col), Some(NodeEnum::List(list))) =
                    (column_name(l), r.and_then(|r| r.node.as_ref()))
                {
                    for item in &list.items {
                        if let Some(n) = param_number(Some(item)) {
                            set(n, column_hint(col.clone()));
                        }
                    }
                }
            }
            NodeRef::InsertStmt(ins) => {
                let Some(rel) = &ins.relation else { continue };
                let table = vec![qualified(&rel.schemaname, &rel.relname)];
                let cols: Vec<Option<String>> = ins
                    .cols
                    .iter()
                    .map(|c| match c.node.as_ref() {
                        Some(NodeEnum::ResTarget(rt)) => Some(rt.name.clone()),
                        _ => None,
                    })
                    .collect();
                let Some(NodeEnum::SelectStmt(sel)) =
                    ins.select_stmt.as_deref().and_then(|s| s.node.as_ref())
                else {
                    continue;
                };
                for row in &sel.values_lists {
                    let Some(NodeEnum::List(list)) = row.node.as_ref() else {
                        continue;
                    };
                    for (i, item) in list.items.iter().enumerate() {
                        let Some(n) = param_number(Some(item)) else {
                            continue;
                        };
                        let column = if cols.is_empty() {
                            Column::Position(i)
                        } else {
                            match cols.get(i) {
                                Some(Some(name)) => Column::Name(name.clone()),
                                _ => continue,
                            }
                        };
                        set(
                            n,
                            ParamHint::Column {
                                tables: table.clone(),
                                column,
                            },
                        );
                    }
                }
            }
            NodeRef::UpdateStmt(upd) => {
                let Some(rel) = &upd.relation else { continue };
                let table = vec![qualified(&rel.schemaname, &rel.relname)];
                for target in &upd.target_list {
                    let Some(NodeEnum::ResTarget(rt)) = target.node.as_ref() else {
                        continue;
                    };
                    if let Some(n) = param_number(rt.val.as_deref()) {
                        set(
                            n,
                            ParamHint::Column {
                                tables: table.clone(),
                                column: Column::Name(rt.name.clone()),
                            },
                        );
                    }
                }
            }
            NodeRef::SelectStmt(sel) => {
                for limit in [sel.limit_count.as_deref(), sel.limit_offset.as_deref()] {
                    if let Some(n) = param_number(limit) {
                        set(n, ParamHint::Int8);
                    }
                }
            }
            _ => {}
        }
    }

    StatementShape {
        params,
        results: result_hints(&parsed),
    }
}

fn result_hints(parsed: &pg_query::ParseResult) -> Vec<Option<ResultHint>> {
    let [raw] = parsed.protobuf.stmts.as_slice() else {
        return vec![];
    };
    let Some(stmt) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
        return vec![];
    };
    let targets: &[Node] = match stmt {
        NodeEnum::SelectStmt(sel) if sel.larg.is_none() && sel.values_lists.is_empty() => {
            &sel.target_list
        }
        NodeEnum::InsertStmt(ins) => &ins.returning_list,
        NodeEnum::UpdateStmt(upd) => &upd.returning_list,
        NodeEnum::DeleteStmt(del) => &del.returning_list,
        _ => return vec![],
    };
    let mut hints = Vec::with_capacity(targets.len());
    for target in targets {
        let Some(NodeEnum::ResTarget(rt)) = target.node.as_ref() else {
            return vec![];
        };
        let val = rt.val.as_deref().and_then(|v| v.node.as_ref());
        if let Some(NodeEnum::ColumnRef(cr)) = val {
            if cr
                .fields
                .iter()
                .any(|f| matches!(f.node, Some(NodeEnum::AStar(_))))
            {
                return vec![];
            }
        }
        hints.push(val.and_then(expression_hint));
    }
    hints
}

fn expression_hint(node: &NodeEnum) -> Option<ResultHint> {
    match node {
        NodeEnum::FuncCall(fc)
            if fc.over.is_none()
                && fc.funcname.last().and_then(string_value).as_deref() == Some("count") =>
        {
            Some(ResultHint::Int8)
        }
        NodeEnum::AExpr(e) => {
            let kind = AExprKind::try_from(e.kind).unwrap_or(AExprKind::Undefined);
            let comparison = matches!(
                e.name.last().and_then(string_value).as_deref(),
                Some("=" | "<>" | "!=" | "<" | ">" | "<=" | ">=" | "~~" | "!~~" | "~~*" | "!~~*")
            );
            match kind {
                AExprKind::AexprOp if comparison => Some(ResultHint::Bool),
                AExprKind::AexprOpAny
                | AExprKind::AexprOpAll
                | AExprKind::AexprDistinct
                | AExprKind::AexprNotDistinct
                | AExprKind::AexprIn
                | AExprKind::AexprLike
                | AExprKind::AexprIlike
                | AExprKind::AexprSimilar
                | AExprKind::AexprBetween
                | AExprKind::AexprNotBetween
                | AExprKind::AexprBetweenSym
                | AExprKind::AexprNotBetweenSym => Some(ResultHint::Bool),
                _ => None,
            }
        }
        NodeEnum::BoolExpr(_) | NodeEnum::NullTest(_) | NodeEnum::BooleanTest(_) => {
            Some(ResultHint::Bool)
        }
        NodeEnum::SubLink(sl) if sl.sub_link_type == 0 /* EXISTS */ => Some(ResultHint::Bool),
        _ => None,
    }
}

/// The highest `$n` in the statement's tokens (0 when there is none or it does not scan).
pub fn param_count(sql: &str) -> usize {
    let Ok(scan) = pg_query::scan(sql) else {
        return 0;
    };
    scan.tokens
        .iter()
        .filter(|t| t.token == Token::Param as i32)
        .filter_map(|t| {
            let start = usize::try_from(t.start).ok()?;
            let end = usize::try_from(t.end).ok()?;
            sql.get(start..end)?
                .strip_prefix('$')?
                .parse::<usize>()
                .ok()
        })
        .max()
        .unwrap_or(0)
}

fn qualified(schema: &str, table: &str) -> String {
    if schema.is_empty() {
        table.to_owned()
    } else {
        format!("{schema}.{table}")
    }
}

fn string_value(node: &Node) -> Option<String> {
    match node.node.as_ref()? {
        NodeEnum::String(s) => Some(s.sval.clone()),
        _ => None,
    }
}

fn param_number(node: Option<&Node>) -> Option<i32> {
    match node?.node.as_ref()? {
        NodeEnum::ParamRef(p) => Some(p.number),
        _ => None,
    }
}

/// The column name of a column reference (`col` or `t.col`).
fn column_name(node: Option<&Node>) -> Option<String> {
    match node?.node.as_ref()? {
        NodeEnum::ColumnRef(cr) => cr.fields.last().and_then(string_value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(tables: &[&str], name: &str) -> ParamHint {
        ParamHint::Column {
            tables: tables.iter().map(|t| t.to_string()).collect(),
            column: Column::Name(name.into()),
        }
    }

    #[test]
    fn counts_the_highest_parameter() {
        assert_eq!(analyze("SELECT 1").params.len(), 0);
        assert_eq!(analyze("SELECT $2, $1").params.len(), 2);
        assert_eq!(analyze("SELECT $3").params.len(), 3);
        assert_eq!(analyze("not sql $1").params.len(), 0);
    }

    #[test]
    fn casts_win() {
        let shape = analyze("SELECT $1::int8 + 1, $2::text[], $3::pg_catalog.int4");
        assert_eq!(
            shape.params,
            vec![
                ParamHint::Cast("int8".into()),
                ParamHint::Cast("text[]".into()),
                ParamHint::Cast("int4".into()),
            ]
        );
    }

    #[test]
    fn column_context() {
        let shape = analyze("SELECT name FROM t WHERE id = $1 AND $2 < qty");
        assert_eq!(shape.params, vec![col(&["t"], "id"), col(&["t"], "qty")]);

        let shape = analyze("UPDATE s.t SET name = $1 WHERE id = $2");
        assert_eq!(
            shape.params,
            vec![col(&["s.t"], "name"), col(&["s.t"], "id")]
        );

        let shape = analyze("INSERT INTO t (id, name) VALUES ($1, $2), ($3, 'x')");
        assert_eq!(
            shape.params,
            vec![col(&["t"], "id"), col(&["t"], "name"), col(&["t"], "id")]
        );

        let shape = analyze("INSERT INTO t VALUES ($1, $2)");
        assert_eq!(
            shape.params[1],
            ParamHint::Column {
                tables: vec!["t".into()],
                column: Column::Position(1)
            }
        );

        let shape = analyze("SELECT * FROM t WHERE id IN ($1, $2) LIMIT $3 OFFSET $4");
        assert_eq!(
            shape.params,
            vec![
                col(&["t"], "id"),
                col(&["t"], "id"),
                ParamHint::Int8,
                ParamHint::Int8
            ]
        );
    }

    #[test]
    fn result_hints() {
        let shape = analyze("SELECT count(*), id = 1, name, id IS NULL, a AND b FROM t");
        assert_eq!(
            shape.results,
            vec![
                Some(ResultHint::Int8),
                Some(ResultHint::Bool),
                None,
                Some(ResultHint::Bool),
                Some(ResultHint::Bool),
            ]
        );
        assert!(analyze("SELECT *, count(*) FROM t").results.is_empty());
        assert_eq!(
            analyze("DELETE FROM t RETURNING id").results,
            vec![None::<ResultHint>]
        );
        assert_eq!(
            analyze("SELECT id + 1 FROM t").results,
            vec![None::<ResultHint>]
        );
    }
}
