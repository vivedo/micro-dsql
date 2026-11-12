//! Optimistic-concurrency bookkeeping of one transaction (M4): which transaction was opened by
//! the server rather than the client, whether a write conflict was deferred, and what is needed
//! to re-open the engine transaction. Plain values only (constitution III).

use crate::classify::StatementClass;
use crate::statement::StatementPlan;

/// OCC state of one transaction; the `occ` field of [`crate::TxnState`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OccTxn {
    /// Opened by micro-dsql (a multi-statement string), not by the client.
    pub implicit: bool,
    /// The client's `BEGIN` / `START TRANSACTION` text, to re-open the transaction.
    pub begin_sql: Option<String>,
    /// The snapshot was fixed by the pin read.
    pub pinned: bool,
    /// A client statement ran after `BEGIN`.
    pub touched: bool,
    /// A data conflict was deferred: `COMMIT` must fail with OC000.
    pub doomed: bool,
}

impl OccTxn {
    /// True when the transaction must be re-opened without the pin before `plan` runs: a DDL
    /// statement as the first statement of a pinned transaction.
    pub fn needs_fresh_snapshot(&self, plan: &StatementPlan) -> bool {
        self.pinned && !self.touched && plan.class == StatementClass::Ddl
    }

    /// Records a deferred conflict. Implicit transactions report conflicts at once, so they
    /// are never doomed.
    pub fn mark_doomed(&mut self) {
        debug_assert!(!self.implicit, "an implicit transaction is never doomed");
        if !self.implicit {
            self.doomed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_a_plain_client_transaction() {
        let occ = OccTxn::default();
        assert!(!occ.implicit && !occ.pinned && !occ.touched && !occ.doomed);
        assert_eq!(occ.begin_sql, None);
    }

    #[test]
    fn a_fresh_snapshot_is_needed_only_for_ddl_first_in_a_pinned_transaction() {
        let ddl = StatementPlan::new("CREATE TABLE a (id int)");
        let dml = StatementPlan::new("INSERT INTO a VALUES (1)");
        let mut occ = OccTxn::default();
        assert!(!occ.needs_fresh_snapshot(&ddl), "not pinned");
        occ.pinned = true;
        assert!(occ.needs_fresh_snapshot(&ddl));
        assert!(!occ.needs_fresh_snapshot(&dml));
        occ.touched = true;
        assert!(!occ.needs_fresh_snapshot(&ddl), "something already ran");
    }

    #[test]
    fn doomed_implies_not_implicit() {
        let mut occ = OccTxn::default();
        occ.mark_doomed();
        assert!(occ.doomed && !occ.implicit);
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn an_implicit_transaction_is_never_doomed() {
        let mut occ = OccTxn {
            implicit: true,
            ..OccTxn::default()
        };
        occ.mark_doomed();
        assert!(!occ.doomed);
    }

    #[test]
    fn doomed_survives_until_the_transaction_ends() {
        let mut occ = OccTxn::default();
        occ.mark_doomed();
        occ.touched = true;
        occ.pinned = true;
        assert!(occ.doomed);
    }
}
