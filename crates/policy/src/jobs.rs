//! DSQL jobs and the `sys.*` forms: job ids, and the facts a plan carries for the server's
//! `sys` and DDL modules (contracts/policy-api-m3.md §2).
//!
//! Nothing here knows about the engine; the server supplies the random bits of a job id and
//! runs what the plan describes.

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
    DropTable {
        schema: Option<String>,
        tables: Vec<String>,
    },
    /// `ALTER TABLE ... DROP COLUMN`: refused when the column is part of the primary key.
    DropColumn {
        schema: Option<String>,
        table: String,
        column: String,
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

#[cfg(test)]
mod tests {
    use super::*;

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
