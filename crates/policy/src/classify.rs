//! Statement classes the transaction rules work with.

use pg_query::protobuf::TransactionStmtKind;
use pg_query::{NodeEnum, NodeRef};

/// Transaction control statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnControl {
    Begin,
    Commit,
    Rollback,
    Savepoint,
    Release,
    RollbackTo,
    Prepare,
    CommitPrepared,
    RollbackPrepared,
}

/// What a statement is, for the DDL/DML discipline of DSQL transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementClass {
    Ddl,
    Dml,
    /// `SELECT` (including `FOR UPDATE`), `EXPLAIN`, `SHOW`-like reads.
    Read,
    Txn(TxnControl),
    /// `SET`, `RESET`, `SHOW`, `DEALLOCATE` and friends.
    Session,
    Other,
}

/// Classifies one parsed statement.
pub fn classify(node: &NodeEnum) -> StatementClass {
    use NodeEnum::*;
    match node {
        InsertStmt(_) | UpdateStmt(_) | DeleteStmt(_) | MergeStmt(_) => StatementClass::Dml,
        SelectStmt(_) => {
            // A data-modifying statement can hide in a WITH clause.
            let modifies = node.nodes().iter().any(|(n, ..)| {
                matches!(
                    n,
                    NodeRef::InsertStmt(_)
                        | NodeRef::UpdateStmt(_)
                        | NodeRef::DeleteStmt(_)
                        | NodeRef::MergeStmt(_)
                )
            });
            if modifies {
                StatementClass::Dml
            } else {
                StatementClass::Read
            }
        }
        ExplainStmt(_) => StatementClass::Read,
        TransactionStmt(t) => StatementClass::Txn(
            match TransactionStmtKind::try_from(t.kind).unwrap_or(TransactionStmtKind::Undefined) {
                TransactionStmtKind::TransStmtBegin | TransactionStmtKind::TransStmtStart => {
                    TxnControl::Begin
                }
                TransactionStmtKind::TransStmtCommit => TxnControl::Commit,
                TransactionStmtKind::TransStmtSavepoint => TxnControl::Savepoint,
                TransactionStmtKind::TransStmtRelease => TxnControl::Release,
                TransactionStmtKind::TransStmtRollbackTo => TxnControl::RollbackTo,
                TransactionStmtKind::TransStmtPrepare => TxnControl::Prepare,
                TransactionStmtKind::TransStmtCommitPrepared => TxnControl::CommitPrepared,
                TransactionStmtKind::TransStmtRollbackPrepared => TxnControl::RollbackPrepared,
                _ => TxnControl::Rollback,
            },
        ),
        VariableSetStmt(_) | VariableShowStmt(_) | DeallocateStmt(_) | DiscardStmt(_) => {
            StatementClass::Session
        }
        CreateStmt(_)
        | IndexStmt(_)
        | AlterTableStmt(_)
        | DropStmt(_)
        | RenameStmt(_)
        | CommentStmt(_)
        | GrantStmt(_)
        | GrantRoleStmt(_)
        | AlterDefaultPrivilegesStmt(_)
        | ViewStmt(_)
        | CreateTableAsStmt(_)
        | CreateSeqStmt(_)
        | AlterSeqStmt(_)
        | CreateFunctionStmt(_)
        | AlterFunctionStmt(_)
        | CreateDomainStmt(_)
        | CreateStatsStmt(_)
        | CreateSchemaStmt(_)
        | CreateRoleStmt(_)
        | AlterRoleStmt(_)
        | DropRoleStmt(_)
        | DefineStmt(_)
        | AlterDomainStmt(_)
        | CreateEnumStmt(_)
        | AlterEnumStmt(_)
        | CompositeTypeStmt(_)
        | CreateRangeStmt(_)
        | CreateExtensionStmt(_)
        | CreateTrigStmt(_)
        | CreatedbStmt(_)
        | DropdbStmt(_)
        | CreateTableSpaceStmt(_)
        | CreateForeignTableStmt(_)
        | AlterObjectSchemaStmt(_)
        | AlterOwnerStmt(_)
        | RuleStmt(_)
        | CreatePublicationStmt(_) => StatementClass::Ddl,
        _ => StatementClass::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(sql: &str) -> StatementClass {
        let parsed = pg_query::parse(sql).unwrap();
        assert_eq!(parsed.protobuf.stmts.len(), 1, "{sql}");
        let node = parsed.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .as_ref()
            .unwrap();
        classify(node)
    }

    #[test]
    fn ddl() {
        for sql in [
            "CREATE TABLE t (id int)",
            "CREATE INDEX i ON t (id)",
            "CREATE UNIQUE INDEX ASYNC_i ON t (id)",
            "ALTER TABLE t ADD COLUMN c int",
            "DROP TABLE t",
            "ALTER TABLE t RENAME TO u",
            "COMMENT ON TABLE t IS 'x'",
            "GRANT SELECT ON t TO u",
            "CREATE VIEW v AS SELECT 1",
            "CREATE TABLE a AS SELECT 1",
            "CREATE SEQUENCE s",
            "ALTER SEQUENCE s RESTART",
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'select 1'",
            "CREATE DOMAIN d AS int",
            "CREATE SCHEMA s CREATE TABLE a (id int) CREATE TABLE b (id int)",
            "DROP INDEX i",
            "CREATE ROLE r",
        ] {
            assert_eq!(class(sql), StatementClass::Ddl, "{sql}");
        }
    }

    #[test]
    fn dml() {
        for sql in [
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE",
            "WITH x AS (INSERT INTO t VALUES (1) RETURNING *) SELECT * FROM x",
            "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x",
            "INSERT INTO t SELECT * FROM s",
        ] {
            assert_eq!(class(sql), StatementClass::Dml, "{sql}");
        }
    }

    #[test]
    fn reads_and_others() {
        for sql in [
            "SELECT 1",
            "SELECT * FROM t FOR UPDATE",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "EXPLAIN SELECT 1",
            "VALUES (1)",
        ] {
            assert_eq!(class(sql), StatementClass::Read, "{sql}");
        }
        for sql in ["SET x = 1", "RESET x", "SHOW x", "DEALLOCATE p"] {
            assert_eq!(class(sql), StatementClass::Session, "{sql}");
        }
        assert_eq!(class("TRUNCATE t"), StatementClass::Other);
        assert_eq!(class("VACUUM"), StatementClass::Other);
    }

    #[test]
    fn transaction_control() {
        for (sql, control) in [
            ("BEGIN", TxnControl::Begin),
            ("START TRANSACTION", TxnControl::Begin),
            ("COMMIT", TxnControl::Commit),
            ("END", TxnControl::Commit),
            ("ROLLBACK", TxnControl::Rollback),
            ("ABORT", TxnControl::Rollback),
            ("SAVEPOINT a", TxnControl::Savepoint),
            ("RELEASE a", TxnControl::Release),
            ("RELEASE SAVEPOINT a", TxnControl::Release),
            ("ROLLBACK TO a", TxnControl::RollbackTo),
            ("ROLLBACK TO SAVEPOINT a", TxnControl::RollbackTo),
            ("PREPARE TRANSACTION 'x'", TxnControl::Prepare),
            ("COMMIT PREPARED 'x'", TxnControl::CommitPrepared),
            ("ROLLBACK PREPARED 'x'", TxnControl::RollbackPrepared),
        ] {
            assert_eq!(class(sql), StatementClass::Txn(control), "{sql}");
        }
    }
}
