//! The M3 deny rules for DDL (contracts/rule-catalogue-m3.md): index DDL, identity and
//! sequences, `ALTER TABLE` forms and the `sys` schema. Stateless, on the AST and the
//! pre-parse result; evaluated after 005's rules, first match wins.

use pg_query::NodeEnum;

use crate::errors::PolicyError;
use crate::rules::RuleContext;

/// One M3 rule: sees the statement, its (blanked) text and the pre-parse context.
pub struct DdlRule {
    pub id: &'static str,
    pub check: fn(&NodeEnum, &str, &RuleContext) -> Option<PolicyError>,
}

/// The first M3 rule that matches.
pub fn check(node: &NodeEnum, sql: &str, ctx: &RuleContext) -> Option<PolicyError> {
    RULES.iter().find_map(|rule| (rule.check)(node, sql, ctx))
}

/// The M3 rules in evaluation order.
pub const RULES: &[DdlRule] = &[];
