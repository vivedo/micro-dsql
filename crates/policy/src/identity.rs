//! Identity columns: `bigint GENERATED ... AS IDENTITY` becomes a sequence plus a default
//! (research.md D5).

use pg_query::protobuf::{ColumnDef, ConstrType, Constraint, DefElem, Node, TypeName};
use pg_query::NodeEnum;

use crate::rewrite::{scan_positions, TextEdit};

/// What desugaring a `CREATE TABLE` with identity columns produces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityPlan {
    /// Edits of the `CREATE TABLE` text.
    pub edits: Vec<TextEdit>,
    /// `CREATE SEQUENCE` statements to run first.
    pub prelude: Vec<String>,
    /// `DROP SEQUENCE IF EXISTS` statements to run if the `CREATE TABLE` fails.
    pub cleanup: Vec<String>,
}

/// The identity constraint of a column, if it has one.
pub(crate) fn identity_constraint(column: &ColumnDef) -> Option<&Constraint> {
    column
        .constraints
        .iter()
        .find_map(|node| match node.node.as_ref()? {
            NodeEnum::Constraint(c) if c.contype == ConstrType::ConstrIdentity as i32 => Some(&**c),
            _ => None,
        })
}

/// The option of a sequence option list (`CREATE SEQUENCE`, `ALTER SEQUENCE`, identity).
pub(crate) fn seq_option<'a>(options: &'a [Node], name: &str) -> Option<&'a DefElem> {
    options.iter().find_map(|node| match node.node.as_ref()? {
        NodeEnum::DefElem(def) if def.defname == name => Some(&**def),
        _ => None,
    })
}

/// The decimal text of a numeric option value (`Integer` or `Float` node).
pub(crate) fn numeric_text(def: &DefElem) -> Option<String> {
    match def.arg.as_ref()?.node.as_ref()? {
        NodeEnum::Integer(i) => Some(i.ival.to_string()),
        NodeEnum::Float(f) => Some(f.fval.clone()),
        _ => None,
    }
}

/// How PostgreSQL displays a column type in error messages.
pub(crate) fn type_display(ty: &TypeName) -> String {
    let name = ty
        .names
        .last()
        .and_then(|n| match n.node.as_ref()? {
            NodeEnum::String(s) => Some(s.sval.as_str()),
            _ => None,
        })
        .unwrap_or("unknown");
    match name {
        "int4" => "integer",
        "int2" => "smallint",
        "int8" => "bigint",
        "bool" => "boolean",
        "float4" => "real",
        "float8" => "double precision",
        other => other,
    }
    .to_owned()
}

fn name_of(node: &Node) -> Option<String> {
    match node.node.as_ref()? {
        NodeEnum::String(s) => Some(s.sval.clone()),
        _ => None,
    }
}

/// An identifier, always quoted.
pub(crate) fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The text inside `nextval('...')` for a sequence: `schema.name`, each part quoted only when
/// it has to be.
pub fn nextval_target(schema: &str, sequence: &str) -> String {
    format!("{}.{}", quote_if_needed(schema), quote_if_needed(sequence))
}

/// The name an identity column's sequence has when the statement does not name it.
pub fn default_sequence_name(table: &str, column: &str) -> String {
    format!("{table}_{column}_seq")
}

/// An identifier, quoted only when it has to be.
fn quote_if_needed(name: &str) -> String {
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
        quote(name)
    }
}

/// Desugars the identity columns of a `CREATE TABLE` (none: an empty plan).
pub fn desugar(node: &NodeEnum, sql: &str) -> IdentityPlan {
    let NodeEnum::CreateStmt(create) = node else {
        return IdentityPlan::default();
    };
    let Some(relation) = create.relation.as_ref() else {
        return IdentityPlan::default();
    };
    let schema = if relation.schemaname.is_empty() {
        "public"
    } else {
        &relation.schemaname
    };
    let mut plan = IdentityPlan::default();
    let toks = scan_positions(sql);
    for element in &create.table_elts {
        let Some(NodeEnum::ColumnDef(column)) = element.node.as_ref() else {
            continue;
        };
        let Some(identity) = identity_constraint(column) else {
            continue;
        };
        let Some(span) = identity_span(&toks, sql, identity.location) else {
            continue;
        };
        // The sequence: its own name (`sequence name`) or `<table>_<column>_seq`.
        let (seq_schema, seq_name) =
            match seq_option(&identity.options, "sequence_name").and_then(|d| {
                match d.arg.as_ref()?.node.as_ref()? {
                    NodeEnum::List(list) => {
                        Some(list.items.iter().filter_map(name_of).collect::<Vec<_>>())
                    }
                    _ => None,
                }
            }) {
                Some(parts) if parts.len() == 2 => (parts[0].clone(), parts[1].clone()),
                Some(parts) if parts.len() == 1 => (schema.to_owned(), parts[0].clone()),
                _ => (
                    schema.to_owned(),
                    default_sequence_name(&relation.relname, &column.colname),
                ),
            };
        let mut create_seq = format!(
            "CREATE SEQUENCE {}{}.{}",
            if create.if_not_exists {
                "IF NOT EXISTS "
            } else {
                ""
            },
            quote(&seq_schema),
            quote(&seq_name)
        );
        for (option, keyword) in [
            ("increment", "INCREMENT BY"),
            ("minvalue", "MINVALUE"),
            ("maxvalue", "MAXVALUE"),
            ("start", "START WITH"),
        ] {
            if let Some(value) = seq_option(&identity.options, option).and_then(numeric_text) {
                create_seq.push_str(&format!(" {keyword} {value}"));
            }
        }
        if seq_option(&identity.options, "cycle").is_some_and(|d| {
            matches!(
                d.arg.as_ref().and_then(|a| a.node.as_ref()),
                Some(NodeEnum::Boolean(b)) if b.boolval
            )
        }) {
            create_seq.push_str(" CYCLE");
        }
        create_seq.push_str(" CACHE 1");
        plan.prelude.push(create_seq);
        if !create.if_not_exists {
            plan.cleanup.push(format!(
                "DROP SEQUENCE IF EXISTS {}.{}",
                quote(&seq_schema),
                quote(&seq_name)
            ));
        }
        // An identity column is NOT NULL; a primary key or an explicit NOT NULL already says so.
        let not_null = !column.constraints.iter().any(|n| {
            matches!(
                n.node.as_ref(),
                Some(NodeEnum::Constraint(c))
                    if c.contype == ConstrType::ConstrPrimary as i32
                        || c.contype == ConstrType::ConstrNotnull as i32
            )
        });
        plan.edits.push(TextEdit {
            at: span.0,
            remove: span.1 - span.0,
            insert: format!(
                "DEFAULT nextval('{}'){}",
                nextval_target(&seq_schema, &seq_name),
                if not_null { " NOT NULL" } else { "" }
            ),
        });
    }
    plan
}

/// The byte span of `GENERATED ... AS IDENTITY [( options )]` starting at `start`.
fn identity_span(
    tokens: &[(pg_query::protobuf::Token, usize, usize)],
    sql: &str,
    start: i32,
) -> Option<(usize, usize)> {
    use pg_query::protobuf::Token;
    let start = usize::try_from(start).ok()?;
    let first = tokens.iter().position(|t| t.1 == start)?;
    let identity = tokens[first..]
        .iter()
        .position(|t| t.0 == Token::IdentityP)?
        + first;
    let mut end = tokens[identity].2;
    let text = |t: &(Token, usize, usize)| &sql[t.1..t.2];
    if tokens.get(identity + 1).is_some_and(|t| text(t) == "(") {
        let mut depth = 0_i32;
        for t in &tokens[identity + 1..] {
            match text(t) {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        end = t.2;
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use crate::statement::StatementPlan;

    fn plan(sql: &str) -> (String, Vec<String>, Vec<String>) {
        let plan = StatementPlan::new(sql);
        assert_eq!(plan.refusal, None, "{sql}");
        (plan.engine_sql, plan.prelude, plan.cleanup_on_failure)
    }

    #[test]
    fn drizzles_form() {
        let (engine, prelude, cleanup) = plan(
            "CREATE TABLE \"users\" (\"id\" bigint PRIMARY KEY GENERATED BY DEFAULT AS IDENTITY \
             (sequence name \"users_id_seq\" INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 \
             START WITH 1 CACHE 65536), \"email\" varchar(255) NOT NULL)",
        );
        assert_eq!(
            prelude,
            [
                r#"CREATE SEQUENCE "public"."users_id_seq" INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 START WITH 1 CACHE 1"#
            ]
        );
        assert_eq!(
            engine,
            "CREATE TABLE \"users\" (\"id\" bigint PRIMARY KEY DEFAULT nextval('public.users_id_seq'), \"email\" varchar(255) NOT NULL)"
        );
        assert_eq!(
            cleanup,
            [r#"DROP SEQUENCE IF EXISTS "public"."users_id_seq""#]
        );
    }

    #[test]
    fn default_names_and_always() {
        let (engine, prelude, cleanup) = plan(
            "CREATE TABLE a (id bigint GENERATED BY DEFAULT AS IDENTITY (CACHE 1) PRIMARY KEY, v text)",
        );
        assert_eq!(prelude, [r#"CREATE SEQUENCE "public"."a_id_seq" CACHE 1"#]);
        assert_eq!(
            engine,
            "CREATE TABLE a (id bigint DEFAULT nextval('public.a_id_seq') PRIMARY KEY, v text)"
        );
        assert_eq!(cleanup, [r#"DROP SEQUENCE IF EXISTS "public"."a_id_seq""#]);
        // GENERATED ALWAYS behaves like BY DEFAULT; an identity column is NOT NULL.
        let (engine, prelude, _) =
            plan("CREATE TABLE t (n bigint GENERATED ALWAYS AS IDENTITY (CACHE 65536), v int)");
        assert_eq!(prelude, [r#"CREATE SEQUENCE "public"."t_n_seq" CACHE 1"#]);
        assert_eq!(
            engine,
            "CREATE TABLE t (n bigint DEFAULT nextval('public.t_n_seq') NOT NULL, v int)"
        );
    }

    #[test]
    fn options_schema_and_several_columns() {
        let (engine, prelude, cleanup) = plan(
            "CREATE TABLE s.t (a bigint GENERATED BY DEFAULT AS IDENTITY (INCREMENT BY 5 START WITH 100 \
             MINVALUE 100 CYCLE CACHE 1), b bigint GENERATED ALWAYS AS IDENTITY (sequence name other_seq CACHE 1) NOT NULL)",
        );
        assert_eq!(
            prelude,
            [
                r#"CREATE SEQUENCE "s"."t_a_seq" INCREMENT BY 5 MINVALUE 100 START WITH 100 CYCLE CACHE 1"#,
                r#"CREATE SEQUENCE "s"."other_seq" CACHE 1"#,
            ]
        );
        assert_eq!(
            engine,
            "CREATE TABLE s.t (a bigint DEFAULT nextval('s.t_a_seq') NOT NULL, b bigint DEFAULT nextval('s.other_seq') NOT NULL)"
        );
        assert_eq!(
            cleanup,
            [
                r#"DROP SEQUENCE IF EXISTS "s"."t_a_seq""#,
                r#"DROP SEQUENCE IF EXISTS "s"."other_seq""#,
            ]
        );
    }

    #[test]
    fn names_that_need_quoting_and_if_not_exists() {
        let (engine, prelude, _) = plan(
            "CREATE TABLE \"My T\" (id bigint GENERATED BY DEFAULT AS IDENTITY (CACHE 1) PRIMARY KEY)",
        );
        assert_eq!(
            prelude,
            [r#"CREATE SEQUENCE "public"."My T_id_seq" CACHE 1"#]
        );
        assert_eq!(
            engine,
            "CREATE TABLE \"My T\" (id bigint DEFAULT nextval('public.\"My T_id_seq\"') PRIMARY KEY)"
        );
        // With IF NOT EXISTS an existing table's sequence must survive: tolerate it, never drop it.
        let (_, prelude, cleanup) = plan(
            "CREATE TABLE IF NOT EXISTS t (id bigint GENERATED BY DEFAULT AS IDENTITY (CACHE 1))",
        );
        assert_eq!(
            prelude,
            [r#"CREATE SEQUENCE IF NOT EXISTS "public"."t_id_seq" CACHE 1"#]
        );
        assert!(cleanup.is_empty());
    }

    #[test]
    fn tables_without_identity_are_untouched() {
        for sql in [
            "CREATE TABLE t (id bigint PRIMARY KEY, n int DEFAULT 1)",
            "SELECT 1",
            "ALTER TABLE t ADD COLUMN c int",
        ] {
            let (engine, prelude, cleanup) = plan(sql);
            assert_eq!(engine, sql);
            assert!(prelude.is_empty() && cleanup.is_empty(), "{sql}");
        }
    }
}
