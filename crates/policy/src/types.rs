//! Column type checks and PostgreSQL's display names for types.

use pg_query::protobuf::TypeName;
use pg_query::NodeEnum;

use crate::errors::PolicyError;

/// Types DSQL does not support as column types (catalog names).
const UNSUPPORTED: &[&str] = &[
    "money",
    "xml",
    "inet",
    "cidr",
    "macaddr",
    "macaddr8",
    "point",
    "line",
    "lseg",
    "box",
    "path",
    "polygon",
    "circle",
    "tsvector",
    "tsquery",
    "int4range",
    "int8range",
    "numrange",
    "tsrange",
    "tstzrange",
    "daterange",
    "bit",
    "varbit",
    "pg_lsn",
];

/// The serial pseudo-types (`type "<name>" does not exist` on DSQL).
const SERIAL: &[&str] = &[
    "serial",
    "serial4",
    "bigserial",
    "serial8",
    "smallserial",
    "serial2",
];

/// The unqualified type name as the parser stored it (`pg_catalog.int4` is `int4`).
fn base_name(ty: &TypeName) -> String {
    ty.names
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            NodeEnum::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .next_back()
        .unwrap_or_default()
}

/// PostgreSQL's display name of a type without array bounds.
fn element_display(name: &str) -> String {
    match name {
        "int2" => "smallint",
        "int4" => "integer",
        "int8" => "bigint",
        "float4" => "real",
        "float8" => "double precision",
        "bool" => "boolean",
        "bpchar" => "character",
        "varchar" => "character varying",
        "varbit" => "bit varying",
        "timestamp" => "timestamp without time zone",
        "timestamptz" => "timestamp with time zone",
        "time" => "time without time zone",
        "timetz" => "time with time zone",
        other => other,
    }
    .to_owned()
}

/// PostgreSQL's display name of a type (`int4` is `integer`, `varbit` is `bit varying`), with
/// `[]` for each array bound.
pub fn display_name(ty: &TypeName) -> String {
    let mut name = element_display(&base_name(ty));
    for _ in &ty.array_bounds {
        name.push_str("[]");
    }
    name
}

/// The refusal for a column of this type, if DSQL does not support it.
pub fn check_column_type(ty: &TypeName) -> Option<PolicyError> {
    let name = base_name(ty);
    if SERIAL.contains(&name.as_str()) {
        return Some(PolicyError::new(
            "serial",
            "42704",
            format!("type \"{name}\" does not exist"),
        ));
    }
    if !ty.array_bounds.is_empty() {
        return Some(PolicyError::new(
            "array_column",
            "0A000",
            format!("datatype {} not supported", display_name(ty)),
        ));
    }
    UNSUPPORTED.contains(&name.as_str()).then(|| {
        PolicyError::new(
            "unsupported_type",
            "0A000",
            format!("datatype {} not supported", display_name(ty)),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column_type(ty: &str) -> TypeName {
        let parsed = pg_query::parse(&format!("CREATE TABLE t (c {ty})")).unwrap();
        let Some(NodeEnum::CreateStmt(create)) =
            parsed.protobuf.stmts[0].stmt.as_ref().unwrap().node.clone()
        else {
            panic!()
        };
        let Some(NodeEnum::ColumnDef(col)) = create.table_elts[0].node.clone() else {
            panic!()
        };
        col.type_name.unwrap()
    }

    #[test]
    fn display_names() {
        for (written, shown) in [
            ("int", "integer"),
            ("int[]", "integer[]"),
            ("text[]", "text[]"),
            ("int[][]", "integer[][]"),
            ("bit(8)", "bit"),
            ("varbit(8)", "bit varying"),
            ("bit varying", "bit varying"),
            ("bigint", "bigint"),
            ("double precision", "double precision"),
            ("money", "money"),
            ("inet", "inet"),
            ("timestamptz", "timestamp with time zone"),
            ("varchar(10)", "character varying"),
            ("boolean", "boolean"),
            ("serial", "serial"),
        ] {
            assert_eq!(display_name(&column_type(written)), shown, "{written}");
        }
    }

    #[test]
    fn unsupported_types_are_named_in_the_message() {
        for (written, shown) in [
            ("money", "money"),
            ("xml", "xml"),
            ("inet", "inet"),
            ("cidr", "cidr"),
            ("macaddr", "macaddr"),
            ("macaddr8", "macaddr8"),
            ("point", "point"),
            ("line", "line"),
            ("lseg", "lseg"),
            ("box", "box"),
            ("path", "path"),
            ("polygon", "polygon"),
            ("circle", "circle"),
            ("tsvector", "tsvector"),
            ("tsquery", "tsquery"),
            ("int4range", "int4range"),
            ("int8range", "int8range"),
            ("numrange", "numrange"),
            ("tsrange", "tsrange"),
            ("tstzrange", "tstzrange"),
            ("daterange", "daterange"),
            ("bit(8)", "bit"),
            ("varbit(8)", "bit varying"),
            ("pg_lsn", "pg_lsn"),
        ] {
            let e = check_column_type(&column_type(written)).unwrap_or_else(|| panic!("{written}"));
            assert_eq!(e.rule, "unsupported_type", "{written}");
            assert_eq!(e.sqlstate, "0A000");
            assert_eq!(
                e.message,
                format!("datatype {shown} not supported"),
                "{written}"
            );
        }
    }

    #[test]
    fn arrays_are_refused_with_the_element_type() {
        for (written, shown) in [
            ("int[]", "integer[]"),
            ("text[]", "text[]"),
            ("uuid[]", "uuid[]"),
        ] {
            let e = check_column_type(&column_type(written)).unwrap();
            assert_eq!(e.rule, "array_column");
            assert_eq!(e.sqlstate, "0A000");
            assert_eq!(e.message, format!("datatype {shown} not supported"));
        }
    }

    #[test]
    fn serial_types_do_not_exist() {
        for name in [
            "serial",
            "serial4",
            "bigserial",
            "serial8",
            "smallserial",
            "serial2",
        ] {
            let e = check_column_type(&column_type(name)).unwrap();
            assert_eq!(e.rule, "serial");
            assert_eq!(e.sqlstate, "42704");
            assert_eq!(e.message, format!("type \"{name}\" does not exist"));
        }
    }

    #[test]
    fn supported_types_pass() {
        for ty in [
            "int",
            "bigint",
            "smallint",
            "numeric(10,2)",
            "numeric",
            "decimal",
            "jsonb",
            "json",
            "uuid",
            "timetz",
            "time",
            "interval",
            "bytea",
            "timestamptz",
            "timestamp",
            "text",
            "varchar(10)",
            "char(3)",
            "boolean",
            "real",
            "double precision",
            "date",
            "oid",
        ] {
            assert_eq!(check_column_type(&column_type(ty)), None, "{ty}");
        }
    }
}
