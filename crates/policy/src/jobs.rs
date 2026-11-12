//! DSQL jobs and the `sys.*` forms: job ids, and the facts a plan carries for the server's
//! `sys` and DDL modules (contracts/policy-api-m3.md §2).
//!
//! Nothing here knows about the engine; the server supplies the random bits of a job id and
//! runs what the plan describes.

use pg_query::protobuf::{AlterTableType, ConstrType, ObjectType, Token};
use pg_query::NodeEnum;

use crate::ddl_rules;
use crate::rewrite::{scan_positions, AsyncKind};

/// Where the job id of a `CALL sys.wait_for_job(...)` comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobIdSource {
    /// A string constant in the statement text.
    Const(String),
    /// The 1-based `$n` parameter.
    Param(usize),
}

/// A statement the server answers through `sys.jobs` (DSQL jobs and `sys.*` forms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobAction {
    /// `CREATE [UNIQUE] INDEX ASYNC ...`: the engine builds the index, then a job row is recorded.
    IndexBuild {
        schema: Option<String>,
        index: Option<String>,
        table: String,
        unique: bool,
        if_not_exists: bool,
    },
    /// `ALTER TABLE ASYNC t VALIDATE CONSTRAINT c`.
    Validate {
        schema: Option<String>,
        table: String,
        constraint: String,
    },
    /// `CALL sys.wait_for_job(<id>)`.
    WaitForJob { id: JobIdSource },
    /// `SELECT [*] FROM sys.dsql_major_version()` and `SELECT sys.dsql_major_version()`.
    MajorVersion { column: String },
}

/// A referential action of a foreign key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FkAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

/// What a `NOT VALID` constraint asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotValidKind {
    ForeignKey {
        column: String,
        ref_schema: Option<String>,
        ref_table: String,
        ref_column: Option<String>,
        on_delete: FkAction,
        on_update: FkAction,
    },
    Check {
        column: String,
        /// The expression text, sliced from the statement.
        expr_sql: String,
    },
}

/// A table named in a statement, schema as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    pub schema: Option<String>,
    pub name: String,
}

/// DDL the server runs with extra work around the engine statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DdlAction {
    /// `ALTER TABLE ... ADD CONSTRAINT ... NOT VALID` (not sent to the engine as text).
    AddConstraintNotValid {
        schema: Option<String>,
        table: String,
        name: String,
        kind: NotValidKind,
    },
    /// `DROP TABLE`: the identity sequences of the tables are dropped with them.
    DropTable { tables: Vec<TableRef> },
    /// `ALTER TABLE ... DROP COLUMN`: refused when a column is part of the primary key.
    DropColumn {
        schema: Option<String>,
        table: String,
        columns: Vec<String>,
    },
}

/// The alphabet of a job id: RFC 4648 base32, lower case, no padding.
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Length of a job id.
pub const JOB_ID_LEN: usize = 26;

/// A job id from 128 random bits: the 16 big-endian bytes in RFC 4648 base32, lower case,
/// without padding (26 characters).
pub fn encode_job_id(bits: u128) -> String {
    // 128 bits make 25 full 5-bit groups and a last group of 3 bits, padded with zeros.
    let mut out = String::with_capacity(JOB_ID_LEN);
    for group in 0..JOB_ID_LEN {
        let shift = 123_i32 - 5 * group as i32;
        let value = if shift >= 0 {
            (bits >> shift) & 31
        } else {
            (bits << -shift) & 31
        };
        out.push(char::from(ALPHABET[value as usize]));
    }
    out
}

/// Whether `id` has the shape of a job id (`^[a-z2-7]{26}$`).
pub fn is_job_id(id: &str) -> bool {
    id.len() == JOB_ID_LEN && id.bytes().all(|b| ALPHABET.contains(&b))
}

/// The [`JobAction`] of a statement, if any.
pub fn job_action(node: &NodeEnum, async_kind: Option<AsyncKind>) -> Option<JobAction> {
    match node {
        NodeEnum::IndexStmt(index) if async_kind == Some(AsyncKind::Index) => {
            let relation = index.relation.as_ref()?;
            Some(JobAction::IndexBuild {
                schema: non_empty(&relation.schemaname),
                index: non_empty(&index.idxname),
                table: relation.relname.clone(),
                unique: index.unique,
                if_not_exists: index.if_not_exists,
            })
        }
        NodeEnum::AlterTableStmt(alter) if async_kind == Some(AsyncKind::AlterTable) => {
            let relation = alter.relation.as_ref()?;
            let [cmd] = alter.cmds.as_slice() else {
                return None;
            };
            let Some(NodeEnum::AlterTableCmd(cmd)) = cmd.node.as_ref() else {
                return None;
            };
            (cmd.subtype == AlterTableType::AtValidateConstraint as i32).then(|| {
                JobAction::Validate {
                    schema: non_empty(&relation.schemaname),
                    table: relation.relname.clone(),
                    constraint: cmd.name.clone(),
                }
            })
        }
        NodeEnum::CallStmt(_) if ddl_rules::is_wait_for_job_call(node) => {
            let NodeEnum::CallStmt(call) = node else {
                return None;
            };
            let arg = call.funccall.as_ref()?.args.first()?;
            let id = match param_number(arg) {
                Some(n) => JobIdSource::Param(n),
                None => JobIdSource::Const(ddl_rules::const_string(arg)?),
            };
            Some(JobAction::WaitForJob { id })
        }
        NodeEnum::SelectStmt(select) => major_version(select),
        _ => None,
    }
}

/// The [`DdlAction`] of a statement, if any.
pub fn ddl_action(node: &NodeEnum, sql: &str) -> Option<DdlAction> {
    match node {
        NodeEnum::AlterTableStmt(alter) => {
            let relation = alter.relation.as_ref()?;
            let schema = non_empty(&relation.schemaname);
            let table = relation.relname.clone();
            let columns: Vec<String> = ddl_rules::alter_cmds(node)
                .filter(|cmd| cmd.subtype == AlterTableType::AtDropColumn as i32)
                .map(|cmd| cmd.name.clone())
                .collect();
            if !columns.is_empty() {
                return Some(DdlAction::DropColumn {
                    schema,
                    table,
                    columns,
                });
            }
            let cmd = ddl_rules::alter_cmds(node)
                .find(|cmd| cmd.subtype == AlterTableType::AtAddConstraint as i32)?;
            let constraint = ddl_rules::constraint(cmd)?;
            not_valid_action(constraint, schema, table, sql)
        }
        NodeEnum::DropStmt(drop) if drop.remove_type == ObjectType::ObjectTable as i32 => {
            let tables = drop
                .objects
                .iter()
                .filter_map(|object| {
                    let Some(NodeEnum::List(list)) = object.node.as_ref() else {
                        return None;
                    };
                    let names: Vec<&str> =
                        list.items.iter().filter_map(ddl_rules::name_part).collect();
                    match names.as_slice() {
                        [name] => Some(TableRef {
                            schema: None,
                            name: (*name).to_owned(),
                        }),
                        [schema, name] => Some(TableRef {
                            schema: Some((*schema).to_owned()),
                            name: (*name).to_owned(),
                        }),
                        _ => None,
                    }
                })
                .collect::<Vec<_>>();
            (!tables.is_empty()).then_some(DdlAction::DropTable { tables })
        }
        _ => None,
    }
}

/// `ADD CONSTRAINT ... NOT VALID` for a form micro-dsql can enforce.
fn not_valid_action(
    c: &pg_query::protobuf::Constraint,
    schema: Option<String>,
    table: String,
    sql: &str,
) -> Option<DdlAction> {
    if !c.skip_validation {
        return None;
    }
    let (name, kind) = if c.contype == ConstrType::ConstrForeign as i32 {
        let [column] = c.fk_attrs.as_slice() else {
            return None;
        };
        let column = ddl_rules::name_part(column)?.to_owned();
        let pk = c.pktable.as_ref()?;
        let action = |code: &str| match code {
            "r" => FkAction::Restrict,
            "c" => FkAction::Cascade,
            "n" => FkAction::SetNull,
            "d" => FkAction::SetDefault,
            _ => FkAction::NoAction,
        };
        let name = if c.conname.is_empty() {
            format!("{table}_{column}_fkey")
        } else {
            c.conname.clone()
        };
        (
            name,
            NotValidKind::ForeignKey {
                column,
                ref_schema: non_empty(&pk.schemaname),
                ref_table: pk.relname.clone(),
                ref_column: c
                    .pk_attrs
                    .first()
                    .and_then(ddl_rules::name_part)
                    .map(str::to_owned),
                on_delete: action(&c.fk_del_action),
                on_update: action(&c.fk_upd_action),
            },
        )
    } else if c.contype == ConstrType::ConstrCheck as i32 {
        let mut columns = ddl_rules::check_columns(c);
        if columns.len() != 1 {
            return None;
        }
        let column = columns.remove(0);
        let name = if c.conname.is_empty() {
            format!("{table}_{column}_check")
        } else {
            c.conname.clone()
        };
        let expr_sql = check_expression(sql, c.location)?;
        (name, NotValidKind::Check { column, expr_sql })
    } else {
        return None;
    };
    Some(DdlAction::AddConstraintNotValid {
        schema,
        table,
        name,
        kind,
    })
}

/// The text between the parentheses of the `CHECK (...)` that starts at or after `from`.
fn check_expression(sql: &str, from: i32) -> Option<String> {
    let from = usize::try_from(from).ok()?;
    let tokens = scan_positions(sql);
    let check = tokens
        .iter()
        .position(|t| t.1 >= from && t.0 == Token::Check)?;
    let open = tokens.get(check + 1).filter(|t| &sql[t.1..t.2] == "(")?;
    let mut depth = 0_i32;
    for t in &tokens[check + 1..] {
        match &sql[t.1..t.2] {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return Some(sql[open.2..t.1].trim().to_owned());
                }
            }
            _ => {}
        }
    }
    None
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

/// `$n` (looking through casts).
fn param_number(node: &pg_query::protobuf::Node) -> Option<usize> {
    match node.node.as_ref()? {
        NodeEnum::ParamRef(p) => usize::try_from(p.number).ok(),
        NodeEnum::TypeCast(cast) => param_number(cast.arg.as_deref()?),
        _ => None,
    }
}

const MAJOR_VERSION_COLUMN: &str = "dsql_major_version";

fn is_major_version_call(call: &pg_query::protobuf::FuncCall) -> bool {
    call.args.is_empty()
        && matches!(
            call.funcname
                .iter()
                .map(ddl_rules::name_part)
                .collect::<Vec<_>>()
                .as_slice(),
            [Some("sys"), Some(MAJOR_VERSION_COLUMN)]
        )
}

/// `SELECT sys.dsql_major_version() [AS x]` and `SELECT * FROM sys.dsql_major_version()`.
fn major_version(select: &pg_query::protobuf::SelectStmt) -> Option<JobAction> {
    let plain = select.where_clause.is_none()
        && select.group_clause.is_empty()
        && select.sort_clause.is_empty()
        && select.with_clause.is_none()
        && select.limit_count.is_none()
        && select.values_lists.is_empty()
        && select.into_clause.is_none();
    if !plain {
        return None;
    }
    let [target] = select.target_list.as_slice() else {
        return None;
    };
    let Some(NodeEnum::ResTarget(target)) = target.node.as_ref() else {
        return None;
    };
    let version = |column: String| Some(JobAction::MajorVersion { column });
    match (
        select.from_clause.as_slice(),
        target.val.as_ref()?.node.as_ref()?,
    ) {
        ([], NodeEnum::FuncCall(call)) if is_major_version_call(call) => {
            version(non_empty(&target.name).unwrap_or_else(|| MAJOR_VERSION_COLUMN.to_owned()))
        }
        ([from], NodeEnum::ColumnRef(_)) => {
            let Some(NodeEnum::RangeFunction(function)) = from.node.as_ref() else {
                return None;
            };
            let [call] = function.functions.as_slice() else {
                return None;
            };
            let Some(NodeEnum::List(call)) = call.node.as_ref() else {
                return None;
            };
            let Some(NodeEnum::FuncCall(call)) = call.items.first()?.node.as_ref() else {
                return None;
            };
            let star = matches!(
                target.val.as_ref()?.node.as_ref()?,
                NodeEnum::ColumnRef(c) if c.fields.len() == 1
                    && matches!(c.fields[0].node, Some(NodeEnum::AStar(_)))
            );
            (is_major_version_call(call) && star).then(|| JobAction::MajorVersion {
                column: MAJOR_VERSION_COLUMN.to_owned(),
            })?;
            version(MAJOR_VERSION_COLUMN.to_owned())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statement::StatementPlan;

    fn job(sql: &str) -> Option<JobAction> {
        StatementPlan::new(sql).job
    }

    fn some(text: &str) -> Option<String> {
        Some(text.to_owned())
    }

    fn ddl(sql: &str) -> Option<DdlAction> {
        StatementPlan::new(sql).ddl
    }

    fn table(schema: Option<&str>, name: &str) -> TableRef {
        TableRef {
            schema: schema.map(str::to_owned),
            name: name.to_owned(),
        }
    }

    #[test]
    fn drop_table_extraction() {
        assert_eq!(
            ddl("DROP TABLE a"),
            Some(DdlAction::DropTable {
                tables: vec![table(None, "a")]
            })
        );
        assert_eq!(
            ddl("DROP TABLE IF EXISTS a, s.\"B b\" CASCADE"),
            Some(DdlAction::DropTable {
                tables: vec![table(None, "a"), table(Some("s"), "B b")]
            })
        );
        assert_eq!(ddl("DROP INDEX i"), None);
        assert_eq!(ddl("DROP SEQUENCE s"), None);
        assert_eq!(ddl("DROP VIEW v"), None);
    }

    #[test]
    fn drop_column_extraction() {
        assert_eq!(
            ddl("ALTER TABLE s.t DROP COLUMN a, DROP COLUMN IF EXISTS b"),
            Some(DdlAction::DropColumn {
                schema: Some("s".into()),
                table: "t".into(),
                columns: vec!["a".into(), "b".into()],
            })
        );
        assert_eq!(ddl("ALTER TABLE t ADD COLUMN a int"), None);
    }

    #[test]
    fn add_constraint_not_valid_extraction() {
        let name = |sql: &str| match ddl(sql) {
            Some(DdlAction::AddConstraintNotValid { name, .. }) => name,
            other => panic!("{sql}: {other:?}"),
        };
        assert_eq!(
            ddl(
                "ALTER TABLE c ADD CONSTRAINT c_fk FOREIGN KEY (parent) REFERENCES s.p (id) \
                 ON DELETE CASCADE ON UPDATE SET NULL NOT VALID"
            ),
            Some(DdlAction::AddConstraintNotValid {
                schema: None,
                table: "c".into(),
                name: "c_fk".into(),
                kind: NotValidKind::ForeignKey {
                    column: "parent".into(),
                    ref_schema: Some("s".into()),
                    ref_table: "p".into(),
                    ref_column: Some("id".into()),
                    on_delete: FkAction::Cascade,
                    on_update: FkAction::SetNull,
                },
            })
        );
        // Drizzle's spelling: explicit "no action", quoted names, no referenced column list.
        assert_eq!(
            ddl(
                "ALTER TABLE \"posts\" ADD CONSTRAINT \"posts_author_id_users_id_fk\" FOREIGN KEY \
                 (\"author_id\") REFERENCES \"public\".\"users\"(\"id\") ON DELETE no action \
                 ON UPDATE no action NOT VALID"
            ),
            Some(DdlAction::AddConstraintNotValid {
                schema: None,
                table: "posts".into(),
                name: "posts_author_id_users_id_fk".into(),
                kind: NotValidKind::ForeignKey {
                    column: "author_id".into(),
                    ref_schema: Some("public".into()),
                    ref_table: "users".into(),
                    ref_column: Some("id".into()),
                    on_delete: FkAction::NoAction,
                    on_update: FkAction::NoAction,
                },
            })
        );
        assert_eq!(
            ddl("ALTER TABLE t ADD CONSTRAINT t_chk CHECK ( a > 0 AND a < 10 ) NOT VALID"),
            Some(DdlAction::AddConstraintNotValid {
                schema: None,
                table: "t".into(),
                name: "t_chk".into(),
                kind: NotValidKind::Check {
                    column: "a".into(),
                    expr_sql: "a > 0 AND a < 10".into(),
                },
            })
        );
        assert_eq!(
            name("ALTER TABLE t ADD CHECK (a > 0) NOT VALID"),
            "t_a_check"
        );
        assert_eq!(
            name("ALTER TABLE t ADD FOREIGN KEY (a) REFERENCES p (id) NOT VALID"),
            "t_a_fkey"
        );
        // Refused forms and valid constraints carry no action.
        assert_eq!(ddl("ALTER TABLE t ADD CONSTRAINT c CHECK (a > 0)"), None);
        assert_eq!(
            ddl("ALTER TABLE t ADD CONSTRAINT c FOREIGN KEY (a, b) REFERENCES p (x, y) NOT VALID"),
            None
        );
    }

    #[test]
    fn index_build_extraction() {
        assert_eq!(
            job("CREATE INDEX ASYNC i ON t (c)"),
            Some(JobAction::IndexBuild {
                schema: None,
                index: some("i"),
                table: "t".into(),
                unique: false,
                if_not_exists: false,
            })
        );
        assert_eq!(
            job("CREATE UNIQUE INDEX ASYNC IF NOT EXISTS \"My Idx\" ON s.t (c)"),
            Some(JobAction::IndexBuild {
                schema: some("s"),
                index: some("My Idx"),
                table: "t".into(),
                unique: true,
                if_not_exists: true,
            })
        );
        assert_eq!(
            job("CREATE INDEX ASYNC ON t (c)"),
            Some(JobAction::IndexBuild {
                schema: None,
                index: None,
                table: "t".into(),
                unique: false,
                if_not_exists: false,
            })
        );
        // A refused index statement has no job.
        assert_eq!(job("CREATE INDEX i ON t (c)"), None);
    }

    #[test]
    fn wait_for_job_extraction() {
        assert_eq!(
            job("CALL sys.wait_for_job('tpqrncdmjja4tdl3zxo2qqvh4y')"),
            Some(JobAction::WaitForJob {
                id: JobIdSource::Const("tpqrncdmjja4tdl3zxo2qqvh4y".into())
            })
        );
        assert_eq!(
            job("CALL sys.wait_for_job($1)"),
            Some(JobAction::WaitForJob {
                id: JobIdSource::Param(1)
            })
        );
        assert_eq!(
            job("call sys.wait_for_job($2::text)"),
            Some(JobAction::WaitForJob {
                id: JobIdSource::Param(2)
            })
        );
        assert_eq!(job("CALL other.proc($1)"), None);
    }

    #[test]
    fn major_version_extraction() {
        let version = |column: &str| {
            Some(JobAction::MajorVersion {
                column: column.into(),
            })
        };
        assert_eq!(
            job("SELECT sys.dsql_major_version()"),
            version("dsql_major_version")
        );
        assert_eq!(job("SELECT sys.dsql_major_version() AS v"), version("v"));
        assert_eq!(job("select sys.dsql_major_version() v"), version("v"));
        assert_eq!(
            job("SELECT * FROM sys.dsql_major_version()"),
            version("dsql_major_version")
        );
        assert_eq!(job("SELECT 1"), None);
        assert_eq!(job("SELECT sys.dsql_major_version() FROM t"), None);
    }

    #[test]
    fn validate_extraction() {
        assert_eq!(
            job("ALTER TABLE ASYNC s.t VALIDATE CONSTRAINT c"),
            Some(JobAction::Validate {
                schema: some("s"),
                table: "t".into(),
                constraint: "c".into()
            })
        );
        assert_eq!(job("ALTER TABLE t VALIDATE CONSTRAINT c"), None);
    }

    #[test]
    fn job_ids_are_26_characters_of_the_base32_alphabet() {
        for bits in [0, 1, u128::MAX, 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210] {
            let id = encode_job_id(bits);
            assert_eq!(id.len(), JOB_ID_LEN, "{id}");
            assert!(id.bytes().all(|b| ALPHABET.contains(&b)), "{id}");
            assert!(is_job_id(&id), "{id}");
        }
    }

    #[test]
    fn encoding_is_rfc_4648_in_byte_order_and_deterministic() {
        assert_eq!(encode_job_id(0), "aaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(encode_job_id(u128::MAX), format!("{}4", "7".repeat(25)));
        // First byte 0x80 = 10000000: first character 10000 = 'q'.
        assert_eq!(&encode_job_id(0x80 << 120)[..2], "qa");
        // Low bits land in the last character: 1 -> 00001 shifted left by two = 4 = 'e'.
        assert_eq!(encode_job_id(1), "aaaaaaaaaaaaaaaaaaaaaaaaae");
        assert_eq!(encode_job_id(12345), encode_job_id(12345));
        assert_ne!(encode_job_id(12345), encode_job_id(12346));
    }

    #[test]
    fn job_id_shape() {
        assert!(is_job_id("tpqrncdmjja4tdl3zxo2qqvh4y"));
        assert!(!is_job_id(""));
        assert!(!is_job_id("tpqrncdmjja4tdl3zxo2qqvh4"));
        assert!(!is_job_id("tpqrncdmjja4tdl3zxo2qqvh4yy"));
        assert!(!is_job_id("TPQRNCDMJJA4TDL3ZXO2QQVH4Y"));
        assert!(!is_job_id("tpqrncdmjja1tdl3zxo2qqvh4y_"));
        assert!(!is_job_id("tpqrncdmjja8tdl3zxo2qqvh4y1"));
        assert!(!is_job_id("no-such-job"));
        assert!(!is_job_id("é".repeat(13).as_str()));
    }
}
