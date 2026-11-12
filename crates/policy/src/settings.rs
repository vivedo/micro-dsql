//! The DSQL session settings whitelist: which parameters can be set, what `SHOW` answers for
//! the ones the engine does not implement, and DSQL's refusals for everything else.

use std::collections::HashMap;

use pg_query::NodeEnum;

use crate::errors::PolicyError;
use crate::intercept::const_text;

/// A parsed SET / RESET / SHOW statement the policy answers or refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingCommand {
    /// `SET [SESSION|LOCAL] name = value` of a remembered setting (value normalised).
    Set {
        name: &'static str,
        value: String,
        local: bool,
    },
    /// `SET name TO DEFAULT`.
    SetDefault { name: &'static str, local: bool },
    /// `RESET name`.
    Reset { name: &'static str },
    /// `RESET ALL`: the engine runs it too; remembered values are cleared on completion.
    ResetAll,
    /// `SHOW name` of a remembered setting.
    Show { name: &'static str },
    /// An accepted no-op (`SET SESSION CHARACTERISTICS ... REPEATABLE READ`); tag `SET`.
    Noop,
}

/// How a whitelisted parameter is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Sent to the engine unchanged (it implements the parameter).
    Forwarded,
    /// Answered by the existing `SET timezone` intercept.
    TimeZone,
    /// Remembered per session for `SHOW`; the engine has no such parameter.
    Remembered,
    /// Remembered and boolean (`enable_*`).
    Bool,
    /// `SET LOCAL role` only.
    Role,
}

struct Def {
    name: &'static str,
    /// Column name of `SHOW`, as PostgreSQL prints it.
    column: &'static str,
    default: &'static str,
    kind: Kind,
    /// Accepts a list of values (`SET search_path = a, b`).
    list: bool,
}

const fn def(name: &'static str, column: &'static str, default: &'static str, kind: Kind) -> Def {
    Def {
        name,
        column,
        default,
        kind,
        list: false,
    }
}

/// The whitelist (FR-030).
const WHITELIST: &[Def] = &[
    def("application_name", "application_name", "", Kind::Forwarded),
    def(
        "client_encoding",
        "client_encoding",
        "UTF8",
        Kind::Forwarded,
    ),
    Def {
        list: true,
        ..def("search_path", "search_path", "", Kind::Forwarded)
    },
    def("timezone", "TimeZone", "UTC", Kind::TimeZone),
    Def {
        list: true,
        ..def("datestyle", "DateStyle", "ISO, MDY", Kind::Remembered)
    },
    def(
        "extra_float_digits",
        "extra_float_digits",
        "1",
        Kind::Remembered,
    ),
    def(
        "intervalstyle",
        "IntervalStyle",
        "postgres",
        Kind::Remembered,
    ),
    def("enable_bitmapscan", "enable_bitmapscan", "on", Kind::Bool),
    def("enable_hashjoin", "enable_hashjoin", "on", Kind::Bool),
    def(
        "enable_indexonlyscan",
        "enable_indexonlyscan",
        "on",
        Kind::Bool,
    ),
    def("enable_indexscan", "enable_indexscan", "on", Kind::Bool),
    def("enable_material", "enable_material", "on", Kind::Bool),
    def("enable_mergejoin", "enable_mergejoin", "on", Kind::Bool),
    def("enable_nestloop", "enable_nestloop", "on", Kind::Bool),
    def("enable_seqscan", "enable_seqscan", "on", Kind::Bool),
    def(
        "dsql.enable_batched_nestloop",
        "dsql.enable_batched_nestloop",
        "on",
        Kind::Bool,
    ),
    def("role", "role", "none", Kind::Role),
];

fn lookup(name: &str) -> Option<&'static Def> {
    let name = name.to_ascii_lowercase();
    // `SET TIME ZONE` is parsed as `timezone`.
    WHITELIST.iter().find(|d| d.name == name)
}

fn not_supported(name: &str) -> PolicyError {
    PolicyError::new(
        "set_not_supported",
        "0A000",
        format!("setting configuration parameter \"{name}\" not supported"),
    )
}

fn set_transaction() -> PolicyError {
    PolicyError::new(
        "set_transaction",
        "0A000",
        "setting configuration parameter \"TRANSACTION\" not supported",
    )
}

/// The refusal for a name that is not on the whitelist: `dsql.*` names are reserved, everything
/// else is "not supported".
fn refuse_name(name: &str) -> PolicyError {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("dsql.") {
        PolicyError::new(
            "dsql_reserved_prefix",
            "42602",
            format!("invalid configuration parameter name \"{lower}\""),
        )
        .with_detail("\"dsql\" is a reserved prefix.")
    } else {
        not_supported(&lower)
    }
}

fn is_unknown_locale(name: &str) -> bool {
    matches!(name, "lc_collate" | "lc_ctype")
}

/// A boolean value as PostgreSQL reads it, normalised to `on` / `off`.
fn parse_bool(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" | "t" | "y" => Some("on"),
        "off" | "false" | "no" | "0" | "f" | "n" => Some("off"),
        _ => None,
    }
}

/// `SHOW DateStyle` prints the style, then the field order, whatever order they were set in
/// (`SET datestyle = DMY` alone keeps the default style).
fn normalise_datestyle(values: &[String]) -> String {
    let (mut style, mut order) = ("ISO", "MDY");
    for value in values
        .iter()
        .flat_map(|v| v.split(|c: char| c == ',' || c.is_whitespace()))
        .filter(|v| !v.is_empty())
    {
        match value.to_ascii_lowercase().as_str() {
            "iso" => style = "ISO",
            "sql" => style = "SQL",
            "postgres" => style = "Postgres",
            "german" => style = "German",
            "dmy" => order = "DMY",
            "mdy" | "us" | "noneuropean" => order = "MDY",
            "ymd" => order = "YMD",
            "euro" | "european" => order = "DMY",
            _ => {}
        }
    }
    format!("{style}, {order}")
}

fn arg_texts(args: &[pg_query::protobuf::Node]) -> Vec<String> {
    args.iter()
        .map(|a| a.node.as_ref().and_then(const_text).unwrap_or_default())
        .collect()
}

impl SettingCommand {
    /// Classifies a parsed statement. `Ok(None)`: not a setting statement, or one that is
    /// forwarded to the engine or to the timezone intercept unchanged.
    pub fn from_ast(node: &NodeEnum) -> Result<Option<SettingCommand>, PolicyError> {
        match node {
            NodeEnum::VariableShowStmt(show) => {
                let name = show.name.to_ascii_lowercase();
                if is_unknown_locale(&name) {
                    return Err(PolicyError::new(
                        "show_unrecognized",
                        "42704",
                        format!("unrecognized configuration parameter \"{name}\""),
                    ));
                }
                Ok(lookup(&name)
                    .filter(|d| matches!(d.kind, Kind::Remembered | Kind::Bool | Kind::Role))
                    .map(|d| SettingCommand::Show { name: d.name }))
            }
            NodeEnum::VariableSetStmt(set) => from_set(set),
            _ => Ok(None),
        }
    }
}

fn from_set(
    set: &pg_query::protobuf::VariableSetStmt,
) -> Result<Option<SettingCommand>, PolicyError> {
    use pg_query::protobuf::VariableSetKind as K;
    let kind = K::try_from(set.kind).unwrap_or(K::Undefined);
    match kind {
        K::VarResetAll => return Ok(Some(SettingCommand::ResetAll)),
        K::VarSetMulti => return from_multi(set),
        _ => {}
    }
    let Some(def) = lookup(&set.name) else {
        return Err(refuse_name(&set.name));
    };
    if def.kind == Kind::TimeZone {
        return Ok(None);
    }
    if def.kind == Kind::Role && !(set.is_local && kind == K::VarSetValue) {
        return Err(not_supported(def.name));
    }
    match kind {
        K::VarSetValue => {
            let values = arg_texts(&set.args);
            if values.len() > 1 && !def.list {
                return Err(PolicyError::new(
                    "set_one_value",
                    "22023",
                    format!("SET {} takes only one argument", def.name),
                ));
            }
            match def.kind {
                Kind::Forwarded => Ok(None),
                Kind::Bool => {
                    let value = values.first().map(String::as_str).unwrap_or_default();
                    let Some(value) = parse_bool(value) else {
                        return Err(PolicyError::new(
                            "set_bool_value",
                            "22023",
                            format!("parameter \"{}\" requires a Boolean value", def.name),
                        ));
                    };
                    Ok(Some(SettingCommand::Set {
                        name: def.name,
                        value: value.to_owned(),
                        local: set.is_local,
                    }))
                }
                _ => Ok(Some(SettingCommand::Set {
                    name: def.name,
                    value: if def.name == "datestyle" {
                        normalise_datestyle(&values)
                    } else {
                        values.join(", ")
                    },
                    local: set.is_local,
                })),
            }
        }
        K::VarSetDefault => Ok(match def.kind {
            Kind::Forwarded => None,
            _ => Some(SettingCommand::SetDefault {
                name: def.name,
                local: set.is_local,
            }),
        }),
        K::VarReset => Ok(match def.kind {
            Kind::Forwarded => None,
            _ => Some(SettingCommand::Reset { name: def.name }),
        }),
        // `SET name FROM CURRENT`: the value does not change.
        _ => Ok(match def.kind {
            Kind::Forwarded => None,
            _ => Some(SettingCommand::Noop),
        }),
    }
}

/// `SET TRANSACTION ...` and `SET SESSION CHARACTERISTICS AS TRANSACTION ...`.
fn from_multi(
    set: &pg_query::protobuf::VariableSetStmt,
) -> Result<Option<SettingCommand>, PolicyError> {
    if !set.name.eq_ignore_ascii_case("session characteristics") {
        return Err(set_transaction());
    }
    for arg in &set.args {
        let Some(NodeEnum::DefElem(def)) = arg.node.as_ref() else {
            continue;
        };
        let value = def
            .arg
            .as_ref()
            .and_then(|a| a.node.as_ref())
            .and_then(const_text)
            .unwrap_or_default();
        match def.defname.as_str() {
            "transaction_isolation" if !value.eq_ignore_ascii_case("repeatable read") => {
                return Err(PolicyError::new(
                    "session_characteristics_level",
                    "0A000",
                    format!(
                        "Unsupported isolation level: {}",
                        value.to_ascii_uppercase()
                    ),
                ));
            }
            "transaction_isolation" => {}
            // READ WRITE is the only access mode DSQL has.
            "transaction_read_only" if value == "0" => {}
            _ => {
                let mut error = set_transaction();
                error.rule = "session_characteristics_mode";
                return Err(error);
            }
        }
    }
    Ok(Some(SettingCommand::Noop))
}

/// Refuses `set_config('<constant name>', ...)` of a non-whitelisted name anywhere in the text.
pub fn check_set_config(sql: &str) -> Option<PolicyError> {
    // Only statements that mention the function can match; skip the scan otherwise.
    if !sql.to_ascii_lowercase().contains("set_config") {
        return None;
    }
    let scanned = pg_query::scan(sql).ok()?;
    let text = |t: &pg_query::protobuf::ScanToken| {
        sql.get(usize::try_from(t.start).ok()?..usize::try_from(t.end).ok()?)
    };
    for w in scanned.tokens.windows(3) {
        let (Some(a), Some(b), Some(c)) = (text(&w[0]), text(&w[1]), text(&w[2])) else {
            continue;
        };
        if w[0].token == pg_query::protobuf::Token::Ident as i32
            && a.trim_matches('"').eq_ignore_ascii_case("set_config")
            && b == "("
            && w[2].token == pg_query::protobuf::Token::Sconst as i32
        {
            let name = c.trim_matches('\'').replace("''", "'");
            if lookup(&name).is_none() {
                return Some(refuse_name(&name));
            }
        }
    }
    None
}

/// The column name of `SHOW <name>` for a remembered setting.
pub fn show_column(name: &str) -> &'static str {
    lookup(name).map_or("", |d| d.column)
}

/// The remembered values of one connection.
#[derive(Debug, Default)]
pub struct SessionSettings {
    values: HashMap<&'static str, String>,
}

impl SessionSettings {
    /// The session value of a whitelisted name, or its default.
    pub fn get(&self, name: &str) -> String {
        let Some(def) = lookup(name) else {
            return String::new();
        };
        self.values
            .get(def.name)
            .cloned()
            .unwrap_or_else(|| def.default.to_owned())
    }

    pub fn set(&mut self, name: &'static str, value: &str) {
        self.values.insert(name, value.to_owned());
    }

    pub fn reset(&mut self, name: &str) {
        if let Some(def) = lookup(name) {
            self.values.remove(def.name);
        }
    }

    pub fn reset_all(&mut self) {
        self.values.clear();
    }
}

/// The default value of a whitelisted name.
pub fn default_value(name: &str) -> String {
    lookup(name).map_or(String::new(), |d| d.default.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(sql: &str) -> Result<Option<SettingCommand>, PolicyError> {
        let parsed = pg_query::parse(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        let node = parsed.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .as_ref()
            .unwrap();
        SettingCommand::from_ast(node)
    }

    fn set(name: &'static str, value: &str, local: bool) -> Option<SettingCommand> {
        Some(SettingCommand::Set {
            name,
            value: value.to_owned(),
            local,
        })
    }

    fn err(sql: &str) -> (&'static str, String, Option<String>) {
        let e = parse(sql).expect_err(sql);
        (e.sqlstate, e.message, e.detail)
    }

    #[test]
    fn remembered_settings_are_set() {
        assert_eq!(
            parse("SET enable_seqscan = off"),
            Ok(set("enable_seqscan", "off", false))
        );
        assert_eq!(
            parse("SET SESSION enable_seqscan TO on"),
            Ok(set("enable_seqscan", "on", false))
        );
        assert_eq!(
            parse("SET LOCAL enable_seqscan = false"),
            Ok(set("enable_seqscan", "off", true))
        );
        assert_eq!(
            parse("SET ENABLE_HASHJOIN = true"),
            Ok(set("enable_hashjoin", "on", false))
        );
        assert_eq!(
            parse("SET enable_nestloop = 0"),
            Ok(set("enable_nestloop", "off", false))
        );
        assert_eq!(
            parse("SET enable_material = 1"),
            Ok(set("enable_material", "on", false))
        );
        assert_eq!(
            parse("SET enable_mergejoin = yes"),
            Ok(set("enable_mergejoin", "on", false))
        );
        assert_eq!(
            parse("SET enable_indexscan = no"),
            Ok(set("enable_indexscan", "off", false))
        );
        assert_eq!(
            parse("SET enable_bitmapscan = off"),
            Ok(set("enable_bitmapscan", "off", false))
        );
        assert_eq!(
            parse("SET enable_indexonlyscan = off"),
            Ok(set("enable_indexonlyscan", "off", false))
        );
        assert_eq!(
            parse("SET dsql.enable_batched_nestloop = off"),
            Ok(set("dsql.enable_batched_nestloop", "off", false))
        );
        assert_eq!(
            parse("SET datestyle = ISO, DMY"),
            Ok(set("datestyle", "ISO, DMY", false))
        );
        assert_eq!(
            parse("SET extra_float_digits = 3"),
            Ok(set("extra_float_digits", "3", false))
        );
        assert_eq!(
            parse("SET intervalstyle = 'sql_standard'"),
            Ok(set("intervalstyle", "sql_standard", false))
        );
    }

    #[test]
    fn default_and_reset() {
        assert_eq!(
            parse("SET enable_seqscan TO DEFAULT"),
            Ok(Some(SettingCommand::SetDefault {
                name: "enable_seqscan",
                local: false
            }))
        );
        assert_eq!(
            parse("RESET enable_seqscan"),
            Ok(Some(SettingCommand::Reset {
                name: "enable_seqscan"
            }))
        );
        assert_eq!(parse("RESET ALL"), Ok(Some(SettingCommand::ResetAll)));
    }

    #[test]
    fn show_of_remembered_settings() {
        for (sql, name) in [
            ("SHOW enable_seqscan", "enable_seqscan"),
            ("SHOW DateStyle", "datestyle"),
            ("SHOW intervalstyle", "intervalstyle"),
            ("SHOW extra_float_digits", "extra_float_digits"),
            (
                "SHOW dsql.enable_batched_nestloop",
                "dsql.enable_batched_nestloop",
            ),
            ("SHOW role", "role"),
        ] {
            assert_eq!(parse(sql), Ok(Some(SettingCommand::Show { name })), "{sql}");
        }
    }

    #[test]
    fn forwarded_and_intercepted_names_are_left_alone() {
        for sql in [
            "SET search_path = a, b",
            "SET search_path TO DEFAULT",
            "RESET search_path",
            "SHOW search_path",
            "SET application_name = 'x'",
            "SET client_encoding = 'UTF8'",
            "SET NAMES 'UTF8'",
            "SET SCHEMA 'public'",
            "SET timezone = 'UTC'",
            "SET TIME ZONE 'Europe/Rome'",
            "SHOW timezone",
            "SELECT 1",
            "SHOW server_version",
        ] {
            assert_eq!(parse(sql), Ok(None), "{sql}");
        }
    }

    #[test]
    fn other_parameters_are_not_supported() {
        for (sql, name) in [
            ("SET work_mem = '64MB'", "work_mem"),
            ("SET SESSION work_mem = '64MB'", "work_mem"),
            ("SET LOCAL work_mem = '64MB'", "work_mem"),
            ("RESET work_mem", "work_mem"),
            ("SET statement_timeout = 1000", "statement_timeout"),
            ("SET SESSION AUTHORIZATION DEFAULT", "session_authorization"),
            ("SET role = x", "role"),
            ("SET SESSION role x", "role"),
            ("RESET role", "role"),
            ("SET ROLE x", "role"),
        ] {
            assert_eq!(
                err(sql),
                (
                    "0A000",
                    format!("setting configuration parameter \"{name}\" not supported"),
                    None
                ),
                "{sql}"
            );
        }
    }

    #[test]
    fn role_is_settable_only_locally() {
        assert_eq!(parse("SET LOCAL role x"), Ok(set("role", "x", true)));
        assert_eq!(parse("SET LOCAL ROLE NONE"), Ok(set("role", "none", true)));
    }

    #[test]
    fn set_transaction_is_refused() {
        for sql in [
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            "SET TRANSACTION READ ONLY",
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ",
        ] {
            assert_eq!(
                err(sql),
                (
                    "0A000",
                    "setting configuration parameter \"TRANSACTION\" not supported".into(),
                    None
                ),
                "{sql}"
            );
        }
    }

    #[test]
    fn session_characteristics() {
        for sql in [
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ WRITE",
        ] {
            assert_eq!(parse(sql), Ok(Some(SettingCommand::Noop)), "{sql}");
        }
        for (sql, level) in [
            ("ISOLATION LEVEL SERIALIZABLE", "SERIALIZABLE"),
            ("ISOLATION LEVEL READ COMMITTED", "READ COMMITTED"),
            ("ISOLATION LEVEL READ UNCOMMITTED", "READ UNCOMMITTED"),
            ("ISOLATION LEVEL SERIALIZABLE, READ WRITE", "SERIALIZABLE"),
        ] {
            assert_eq!(
                err(&format!("SET SESSION CHARACTERISTICS AS TRANSACTION {sql}")),
                (
                    "0A000",
                    format!("Unsupported isolation level: {level}"),
                    None
                ),
                "{sql}"
            );
        }
        for mode in [
            "READ ONLY",
            "DEFERRABLE",
            "NOT DEFERRABLE",
            "ISOLATION LEVEL REPEATABLE READ, READ ONLY",
        ] {
            assert_eq!(
                err(&format!(
                    "SET SESSION CHARACTERISTICS AS TRANSACTION {mode}"
                )),
                (
                    "0A000",
                    "setting configuration parameter \"TRANSACTION\" not supported".into(),
                    None
                ),
                "{mode}"
            );
        }
    }

    #[test]
    fn unknown_dsql_names_are_reserved() {
        for sql in [
            "SET dsql.foo = 1",
            "RESET dsql.foo",
            "SET LOCAL dsql.bar = 'x'",
            "SET DSQL.foo = 1",
        ] {
            let (code, message, detail) = err(sql);
            assert_eq!(code, "42602", "{sql}");
            assert!(
                message.starts_with("invalid configuration parameter name \"dsql."),
                "{sql}: {message}"
            );
            assert_eq!(
                detail.as_deref(),
                Some("\"dsql\" is a reserved prefix."),
                "{sql}"
            );
        }
        assert_eq!(
            err("SET dsql.foo = 1").1,
            "invalid configuration parameter name \"dsql.foo\""
        );
    }

    #[test]
    fn booleans_are_validated() {
        assert_eq!(
            err("SET enable_seqscan = banana"),
            (
                "22023",
                "parameter \"enable_seqscan\" requires a Boolean value".into(),
                None
            )
        );
        assert_eq!(
            err("SET dsql.enable_batched_nestloop = 2"),
            (
                "22023",
                "parameter \"dsql.enable_batched_nestloop\" requires a Boolean value".into(),
                None
            )
        );
        assert_eq!(
            err("SET enable_hashjoin = 'maybe'").1,
            "parameter \"enable_hashjoin\" requires a Boolean value"
        );
    }

    #[test]
    fn value_lists_are_refused_except_search_path_and_datestyle() {
        for name in [
            "enable_seqscan",
            "dsql.enable_batched_nestloop",
            "extra_float_digits",
            "intervalstyle",
            "application_name",
            "client_encoding",
        ] {
            assert_eq!(
                err(&format!("SET {name} = 'on', 'off'")),
                ("22023", format!("SET {name} takes only one argument"), None),
                "{name}"
            );
        }
        assert!(parse("SET search_path = a, b").is_ok());
        assert!(parse("SET datestyle = ISO, MDY").is_ok());
    }

    #[test]
    fn show_of_locale_parameters_is_unrecognized() {
        for name in ["lc_collate", "lc_ctype"] {
            assert_eq!(
                err(&format!("SHOW {name}")),
                (
                    "42704",
                    format!("unrecognized configuration parameter \"{name}\""),
                    None
                )
            );
        }
        assert_eq!(
            err("SHOW LC_COLLATE").1,
            "unrecognized configuration parameter \"lc_collate\""
        );
    }

    #[test]
    fn set_config_with_a_constant_name() {
        let check = |sql: &str| check_set_config(sql);
        let e = check("SELECT set_config('work_mem', '1MB', false)").unwrap();
        assert_eq!(
            (e.rule, e.sqlstate, e.message.as_str()),
            (
                "set_not_supported",
                "0A000",
                "setting configuration parameter \"work_mem\" not supported"
            )
        );
        let e = check("SELECT SET_CONFIG('WORK_MEM', '1MB', false)").unwrap();
        assert_eq!(
            e.message,
            "setting configuration parameter \"work_mem\" not supported"
        );
        let e = check("SELECT 1 WHERE pg_catalog.set_config('dsql.foo', '1', false) IS NOT NULL")
            .unwrap();
        assert_eq!(e.sqlstate, "42602");
        assert_eq!(e.detail.as_deref(), Some("\"dsql\" is a reserved prefix."));
        assert_eq!(
            check("SELECT set_config('enable_seqscan', 'off', false)"),
            None
        );
        assert_eq!(check("SELECT set_config('search_path', 'a', false)"), None);
        assert_eq!(check("SELECT set_config($1, 'x', false)"), None);
        assert_eq!(check("SELECT 'set_config(''work_mem''' "), None);
        assert_eq!(check("SELECT 1"), None);
    }

    #[test]
    fn session_settings_defaults_and_values() {
        let mut s = SessionSettings::default();
        assert_eq!(s.get("datestyle"), "ISO, MDY");
        assert_eq!(s.get("intervalstyle"), "postgres");
        assert_eq!(s.get("extra_float_digits"), "1");
        assert_eq!(s.get("enable_seqscan"), "on");
        assert_eq!(s.get("dsql.enable_batched_nestloop"), "on");
        assert_eq!(s.get("role"), "none");
        s.set("enable_seqscan", "off");
        assert_eq!(s.get("enable_seqscan"), "off");
        s.reset("enable_seqscan");
        assert_eq!(s.get("enable_seqscan"), "on");
        s.set("datestyle", "ISO, DMY");
        s.set("enable_material", "off");
        s.reset_all();
        assert_eq!(s.get("datestyle"), "ISO, MDY");
        assert_eq!(s.get("enable_material"), "on");
    }

    #[test]
    fn show_columns_are_named_like_postgres() {
        for (name, column) in [
            ("datestyle", "DateStyle"),
            ("intervalstyle", "IntervalStyle"),
            ("extra_float_digits", "extra_float_digits"),
            ("enable_seqscan", "enable_seqscan"),
            (
                "dsql.enable_batched_nestloop",
                "dsql.enable_batched_nestloop",
            ),
            ("role", "role"),
        ] {
            assert_eq!(show_column(name), column);
        }
    }
}
