//! Identity columns: `bigint GENERATED ... AS IDENTITY` becomes a sequence plus a default
//! (research.md D5).

use pg_query::NodeEnum;

use crate::rewrite::TextEdit;

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

/// Desugars the identity columns of a `CREATE TABLE` (none: an empty plan).
pub fn desugar(_node: &NodeEnum, _sql: &str) -> IdentityPlan {
    IdentityPlan::default()
}
