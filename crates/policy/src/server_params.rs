//! DSQL-shaped server parameters.
//!
//! Values follow Aurora DSQL as documented and as measured by other emulators
//! (research/01 §1, research/03): PostgreSQL 16, UTF-8, UTC, no superuser.

/// The `server_version` reported to clients. DSQL's user guide shows `16.13`.
pub const SERVER_VERSION: &str = "16.13";

/// `server_version_num` matching [`SERVER_VERSION`].
pub const SERVER_VERSION_NUM: &str = "160013";

/// The only time zone a session has.
pub const TIME_ZONE: &str = "UTC";

/// The only isolation level DSQL offers.
pub const ISOLATION_LEVEL: &str = "repeatable read";

/// Parameters sent as ParameterStatus after authentication, in this order.
/// `application_name` and `session_authorization` come from the client and are added by the
/// server.
pub const STARTUP_PARAMETERS: &[(&str, &str)] = &[
    ("server_version", SERVER_VERSION),
    ("server_encoding", "UTF8"),
    ("client_encoding", "UTF8"),
    ("DateStyle", "ISO, MDY"),
    ("IntervalStyle", "postgres"),
    ("TimeZone", TIME_ZONE),
    ("integer_datetimes", "on"),
    ("is_superuser", "off"),
    ("standard_conforming_strings", "on"),
    ("in_hot_standby", "off"),
    ("default_transaction_read_only", "off"),
    ("scram_iterations", "4096"),
];

/// Parameters the server answers for `SHOW <name>` itself, with the column name PostgreSQL
/// uses and the value. Names are matched case-insensitively. Parameters not listed here go to
/// the engine (`search_path`, `DateStyle`, `client_encoding`, ...).
pub const SHOW_PARAMETERS: &[(&str, &str)] = &[
    ("server_version", SERVER_VERSION),
    ("server_version_num", SERVER_VERSION_NUM),
    ("TimeZone", TIME_ZONE),
    ("is_superuser", "off"),
    ("server_encoding", "UTF8"),
    ("integer_datetimes", "on"),
    ("in_hot_standby", "off"),
    ("transaction_isolation", ISOLATION_LEVEL),
    ("default_transaction_isolation", ISOLATION_LEVEL),
];

/// The `(column name, value)` for `SHOW <name>`, if the server answers it itself.
pub fn show_parameter(name: &str) -> Option<(&'static str, &'static str)> {
    SHOW_PARAMETERS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
        .copied()
}

/// Whether `value` names UTC (accepted by `SET timezone`).
pub fn is_utc_spelling(value: &str) -> bool {
    const UTC: &[&str] = &[
        "UTC",
        "Etc/UTC",
        "UCT",
        "Etc/UCT",
        "GMT",
        "Etc/GMT",
        "Z",
        "Zulu",
        "UTC0",
        "GMT0",
        "Universal",
        "Etc/Universal",
        "+00",
        "+00:00",
        "00:00",
        "0",
    ];
    let value = value.trim();
    UTC.iter().any(|utc| utc.eq_ignore_ascii_case(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_parameters_look_like_dsql() {
        let get = |name: &str| {
            STARTUP_PARAMETERS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| *v)
        };
        assert!(get("server_version").unwrap().starts_with("16."));
        assert_eq!(get("TimeZone"), Some("UTC"));
        assert_eq!(get("is_superuser"), Some("off"));
        assert_eq!(get("server_encoding"), Some("UTF8"));
    }

    #[test]
    fn show_is_case_insensitive_and_uses_pg_column_names() {
        assert_eq!(show_parameter("timezone"), Some(("TimeZone", "UTC")));
        assert_eq!(
            show_parameter("SERVER_VERSION").unwrap().0,
            "server_version"
        );
        assert_eq!(show_parameter("search_path"), None);
        // DSQL has no such parameters: SHOW fails with 42704 (see `settings`).
        assert_eq!(show_parameter("lc_collate"), None);
        assert_eq!(show_parameter("lc_ctype"), None);
    }

    #[test]
    fn utc_spellings() {
        for v in ["UTC", "utc", "Etc/UTC", "GMT", " UTC "] {
            assert!(is_utc_spelling(v), "{v}");
        }
        for v in ["Europe/Rome", "PST8PDT", "+01"] {
            assert!(!is_utc_spelling(v), "{v}");
        }
    }
}
