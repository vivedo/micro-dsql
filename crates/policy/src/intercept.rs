//! Statements the server answers without the engine.

use pg_query::protobuf::{a_const, VariableSetKind};
use pg_query::NodeEnum;

use crate::server_params::{is_utc_spelling, show_parameter};

/// A statement the server answers itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intercept {
    /// `SHOW <name>`: one row, one text column.
    Show {
        column: &'static str,
        value: &'static str,
    },
    /// A `SET`/`RESET` accepted as a no-op; the command tag is `SET` or `RESET`.
    SetNoop { tag: &'static str },
    /// A statement rejected with an error.
    Error {
        sqlstate: &'static str,
        message: String,
    },
}

/// Classifies one SQL statement. Returns `None` for anything the engine should run, including
/// text that does not parse (the engine reports the syntax error).
pub fn intercept(sql: &str) -> Option<Intercept> {
    let parsed = pg_query::parse(sql).ok()?;
    let [raw] = parsed.protobuf.stmts.as_slice() else {
        return None;
    };
    match raw.stmt.as_ref()?.node.as_ref()? {
        NodeEnum::VariableShowStmt(show) => {
            show_parameter(&show.name).map(|(column, value)| Intercept::Show { column, value })
        }
        NodeEnum::VariableSetStmt(set) if set.name.eq_ignore_ascii_case("timezone") => {
            let kind = VariableSetKind::try_from(set.kind).unwrap_or(VariableSetKind::Undefined);
            match kind {
                VariableSetKind::VarSetDefault | VariableSetKind::VarSetCurrent => {
                    Some(Intercept::SetNoop { tag: "SET" })
                }
                VariableSetKind::VarReset => Some(Intercept::SetNoop { tag: "RESET" }),
                VariableSetKind::VarSetValue => {
                    let value = match set.args.as_slice() {
                        [arg] => const_text(arg.node.as_ref()?)?,
                        _ => return None,
                    };
                    if is_utc_spelling(&value) {
                        Some(Intercept::SetNoop { tag: "SET" })
                    } else {
                        Some(Intercept::Error {
                            sqlstate: "22023",
                            message: format!(
                                "invalid value for parameter \"TimeZone\": \"{value}\"; \
                                 supported value is \"UTC\""
                            ),
                        })
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn const_text(node: &NodeEnum) -> Option<String> {
    let NodeEnum::AConst(c) = node else {
        return None;
    };
    match c.val.as_ref()? {
        a_const::Val::Sval(s) => Some(s.sval.clone()),
        a_const::Val::Ival(i) => Some(i.ival.to_string()),
        a_const::Val::Fval(f) => Some(f.fval.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_known_parameters() {
        assert_eq!(
            intercept("SHOW server_version"),
            Some(Intercept::Show {
                column: "server_version",
                value: crate::server_params::SERVER_VERSION
            })
        );
        assert_eq!(
            intercept("show TimeZone;"),
            Some(Intercept::Show {
                column: "TimeZone",
                value: "UTC"
            })
        );
        assert_eq!(
            intercept("SHOW TIME ZONE"),
            Some(Intercept::Show {
                column: "TimeZone",
                value: "UTC"
            })
        );
    }

    #[test]
    fn other_statements_pass_through() {
        assert_eq!(intercept("SHOW search_path"), None);
        assert_eq!(intercept("SELECT 1"), None);
        assert_eq!(intercept("SELEC 1"), None);
        assert_eq!(intercept("SHOW server_version; SELECT 1"), None);
        assert_eq!(intercept("SET search_path = public"), None);
    }

    #[test]
    fn set_timezone_utc_is_a_noop() {
        for sql in [
            "SET timezone = 'UTC'",
            "SET TIME ZONE 'UTC'",
            "SET SESSION timezone TO utc",
            "SET LOCAL TIME ZONE 'Etc/UTC'",
            "SET TIME ZONE 0",
            "SET timezone TO DEFAULT",
        ] {
            assert_eq!(
                intercept(sql),
                Some(Intercept::SetNoop { tag: "SET" }),
                "{sql}"
            );
        }
        assert_eq!(
            intercept("RESET timezone"),
            Some(Intercept::SetNoop { tag: "RESET" })
        );
    }

    #[test]
    fn set_timezone_other_values_are_rejected() {
        match intercept("SET timezone = 'Europe/Rome'") {
            Some(Intercept::Error { sqlstate, .. }) => assert_eq!(sqlstate, "22023"),
            other => panic!("{other:?}"),
        }
    }
}
