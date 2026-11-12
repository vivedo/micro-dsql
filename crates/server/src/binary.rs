//! Binary-format parameters and results (M0 blockers B2 and B3).
//!
//! The engine stores text for dates, timestamps, UUIDs and JSON, integers and floats for
//! numbers. These functions translate between those values and PostgreSQL's binary wire
//! encodings for the common types.

use pgwire::api::Type;
use pgwire::error::{PgWireError, PgWireResult};
use turso_core::{Numeric, Value};

/// Microseconds between 1970-01-01 and 2000-01-01 (the PostgreSQL epoch).
const PG_EPOCH_DAYS: i64 = 10_957;
const USECS_PER_DAY: i64 = 86_400_000_000;

fn invalid(type_name: &str, detail: &str) -> PgWireError {
    PgWireError::UserError(Box::new(pgwire::error::ErrorInfo::new(
        "ERROR".to_owned(),
        "22P03".to_owned(),
        format!("incorrect binary data format for type {type_name}: {detail}"),
    )))
}

fn fixed<const N: usize>(bytes: &[u8], type_name: &str) -> PgWireResult<[u8; N]> {
    bytes.try_into().map_err(|_| {
        invalid(
            type_name,
            &format!("expected {N} bytes, got {}", bytes.len()),
        )
    })
}

/// Decode one binary-format parameter. `pg_type` is the declared or inferred type; when it
/// is unknown the server described the parameter as `text`, so the bytes are text.
pub(crate) fn decode_param(bytes: &[u8], pg_type: Option<&Type>) -> PgWireResult<Value> {
    let Some(pg_type) = pg_type else {
        return super::pg_bytes_to_value(bytes, &Type::UNKNOWN);
    };
    let text = |bytes: &[u8]| -> PgWireResult<Value> {
        let s = std::str::from_utf8(bytes).map_err(|e| invalid(pg_type.name(), &e.to_string()))?;
        Ok(Value::from_text(s.to_owned()))
    };
    let value = match *pg_type {
        Type::INT2 => Value::from_i64(i16::from_be_bytes(fixed(bytes, "smallint")?).into()),
        Type::INT4 => Value::from_i64(i32::from_be_bytes(fixed(bytes, "integer")?).into()),
        Type::INT8 => Value::from_i64(i64::from_be_bytes(fixed(bytes, "bigint")?)),
        Type::OID => Value::from_i64(u32::from_be_bytes(fixed(bytes, "oid")?).into()),
        Type::FLOAT4 => Value::from_f64(f32::from_be_bytes(fixed(bytes, "real")?).into()),
        Type::FLOAT8 => Value::from_f64(f64::from_be_bytes(fixed(bytes, "double precision")?)),
        Type::BOOL => Value::from_i64(i64::from(fixed::<1>(bytes, "boolean")?[0] != 0)),
        Type::NUMERIC => {
            let s = decode_numeric(bytes)?;
            match s.parse::<f64>() {
                Ok(f) if s != "NaN" => Value::from_f64(f),
                _ => Value::from_text(s),
            }
        }
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::JSON | Type::UNKNOWN => {
            text(bytes)?
        }
        Type::JSONB => match bytes.split_first() {
            Some((1, rest)) => text(rest)?,
            _ => return Err(invalid("jsonb", "unsupported jsonb version")),
        },
        Type::BYTEA => Value::from_blob(bytes.to_vec()),
        Type::UUID => Value::from_text(format_uuid(&fixed::<16>(bytes, "uuid")?)),
        Type::DATE => {
            let days = i32::from_be_bytes(fixed(bytes, "date")?);
            Value::from_text(match days {
                i32::MAX => "infinity".to_owned(),
                i32::MIN => "-infinity".to_owned(),
                d => format_date(i64::from(d) + PG_EPOCH_DAYS),
            })
        }
        Type::TIMESTAMP | Type::TIMESTAMPTZ => {
            let usecs = i64::from_be_bytes(fixed(bytes, pg_type.name())?);
            Value::from_text(match usecs {
                i64::MAX => "infinity".to_owned(),
                i64::MIN => "-infinity".to_owned(),
                u => {
                    let mut s = format_timestamp(u);
                    if *pg_type == Type::TIMESTAMPTZ {
                        s.push_str("+00:00");
                    }
                    s
                }
            })
        }
        _ => {
            return Err(invalid(
                pg_type.name(),
                "binary format is not supported for this type; send it as text",
            ))
        }
    };
    Ok(value)
}

/// Encode a result value in binary for `pg_type`, or `None` to fall back to the generic
/// encoder (text, bytea, NULL and anything not handled here).
pub(crate) fn encode_result(val: &Value, pg_type: &Type) -> Option<Vec<u8>> {
    let as_i64 = || -> Option<i64> {
        match val {
            Value::Numeric(Numeric::Integer(i)) => Some(*i),
            Value::Numeric(Numeric::Float(f)) => Some(f64::from(*f) as i64),
            Value::Text(t) => t.value.trim().parse().ok(),
            _ => None,
        }
    };
    let as_f64 = || -> Option<f64> {
        match val {
            Value::Numeric(Numeric::Integer(i)) => Some(*i as f64),
            Value::Numeric(Numeric::Float(f)) => Some(f64::from(*f)),
            Value::Text(t) => t.value.trim().parse().ok(),
            _ => None,
        }
    };
    let as_text = || -> Option<String> {
        match val {
            Value::Numeric(Numeric::Integer(i)) => Some(i.to_string()),
            Value::Numeric(Numeric::Float(f)) => Some(f64::from(*f).to_string()),
            Value::Text(t) => Some(t.value.to_string()),
            _ => None,
        }
    };
    if matches!(val, Value::Null) {
        return None;
    }
    match *pg_type {
        Type::INT2 => Some(i16::try_from(as_i64()?).ok()?.to_be_bytes().to_vec()),
        Type::INT4 => Some(i32::try_from(as_i64()?).ok()?.to_be_bytes().to_vec()),
        Type::INT8 => Some(as_i64()?.to_be_bytes().to_vec()),
        Type::FLOAT4 => Some((as_f64()? as f32).to_be_bytes().to_vec()),
        Type::FLOAT8 => Some(as_f64()?.to_be_bytes().to_vec()),
        Type::BOOL => match val {
            Value::Numeric(Numeric::Integer(i)) => Some(vec![u8::from(*i != 0)]),
            Value::Text(t) => match t.value.as_ref() {
                "t" | "true" => Some(vec![1]),
                "f" | "false" => Some(vec![0]),
                _ => None,
            },
            _ => None,
        },
        Type::NUMERIC => encode_numeric(&as_text()?),
        Type::UUID => match val {
            Value::Text(t) => parse_uuid(&t.value).map(|u| u.to_vec()),
            Value::Blob(b) if b.len() == 16 => Some(b.to_vec()),
            _ => None,
        },
        Type::DATE => {
            let Value::Text(t) = val else { return None };
            let (days, rest) = parse_date(t.value.trim())?;
            if !rest.is_empty() {
                return None;
            }
            Some(
                i32::try_from(days - PG_EPOCH_DAYS)
                    .ok()?
                    .to_be_bytes()
                    .to_vec(),
            )
        }
        Type::TIMESTAMP | Type::TIMESTAMPTZ => {
            let Value::Text(t) = val else { return None };
            Some(parse_timestamp(t.value.trim())?.to_be_bytes().to_vec())
        }
        Type::JSONB => {
            let mut out = vec![1];
            out.extend_from_slice(as_text()?.as_bytes());
            Some(out)
        }
        _ => None,
    }
}

fn format_uuid(b: &[u8; 16]) -> String {
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn parse_uuid(s: &str) -> Option<[u8; 16]> {
    let hex: Vec<u8> = s
        .trim()
        .trim_matches(|c| c == '{' || c == '}')
        .bytes()
        .filter(|c| *c != b'-')
        .collect();
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, pair) in hex.as_chunks::<2>().0.iter().enumerate() {
        let s = std::str::from_utf8(pair).ok()?;
        out[i] = u8::from_str_radix(s, 16).ok()?;
    }
    Some(out)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `days` since 1970-01-01 as `YYYY-MM-DD`.
fn format_date(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Microseconds since 2000-01-01 as `YYYY-MM-DD HH:MM:SS[.ffffff]`.
fn format_timestamp(usecs: i64) -> String {
    let days = usecs.div_euclid(USECS_PER_DAY);
    let mut rem = usecs.rem_euclid(USECS_PER_DAY);
    let micros = rem % 1_000_000;
    rem /= 1_000_000;
    let (h, mi, s) = (rem / 3600, (rem / 60) % 60, rem % 60);
    let mut out = format!(
        "{} {h:02}:{mi:02}:{s:02}",
        format_date(days + PG_EPOCH_DAYS)
    );
    if micros != 0 {
        let frac = format!("{micros:06}");
        out.push('.');
        out.push_str(frac.trim_end_matches('0'));
    }
    out
}

/// Parses `YYYY-MM-DD` at the start of `s`; returns days since 1970-01-01 and the rest.
fn parse_date(s: &str) -> Option<(i64, &str)> {
    let mut parts = s.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let rest = parts.next()?;
    let day_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    let d: i64 = rest[..day_len].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((days_from_civil(y, m, d), &rest[day_len..]))
}

/// Parses `YYYY-MM-DD[( |T)HH:MM[:SS[.f]]][Z|±HH[:MM[:SS]]]` to microseconds since
/// 2000-01-01 UTC.
fn parse_timestamp(s: &str) -> Option<i64> {
    let (days, rest) = parse_date(s)?;
    let mut usecs = (days - PG_EPOCH_DAYS) * USECS_PER_DAY;
    let rest = rest.trim_start_matches([' ', 'T']);
    if rest.is_empty() {
        return Some(usecs);
    }
    let tz_at = rest
        .char_indices()
        .skip(1)
        .find(|(_, c)| matches!(c, '+' | '-' | 'Z' | 'z'))
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    let (time, tz) = rest.split_at(tz_at);
    let mut fields = time.trim().split(':');
    let h: i64 = fields.next()?.parse().ok()?;
    let mi: i64 = fields.next()?.parse().ok()?;
    let (sec, frac) = match fields.next() {
        Some(sec) => match sec.split_once('.') {
            Some((s, f)) => (s.parse::<i64>().ok()?, f),
            None => (sec.parse::<i64>().ok()?, ""),
        },
        None => (0, ""),
    };
    let frac_usecs: i64 = if frac.is_empty() {
        0
    } else {
        let digits: String = frac.chars().take(6).collect();
        digits.parse::<i64>().ok()? * 10_i64.pow(6 - digits.len() as u32)
    };
    usecs += ((h * 60 + mi) * 60 + sec) * 1_000_000 + frac_usecs;
    let tz = tz.trim();
    if !tz.is_empty() && !tz.eq_ignore_ascii_case("z") {
        let sign = if tz.starts_with('-') { -1 } else { 1 };
        let mut parts = tz[1..].split(':');
        let first = parts.next()?;
        let (oh, om) = if first.len() == 4 && !first.contains(':') {
            (
                first[..2].parse::<i64>().ok()?,
                first[2..].parse::<i64>().ok()?,
            )
        } else {
            (
                first.parse::<i64>().ok()?,
                parts.next().map_or(Some(0), |m| m.parse().ok())?,
            )
        };
        let os: i64 = parts.next().map_or(Some(0), |s| s.parse().ok())?;
        usecs -= sign * ((oh * 60 + om) * 60 + os) * 1_000_000;
    }
    Some(usecs)
}

/// PostgreSQL binary `numeric` from a decimal string (`-12.340`, `NaN`).
fn encode_numeric(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    let header = |ndigits: u16, weight: i16, sign: u16, dscale: u16| {
        let mut out = Vec::with_capacity(8 + 2 * usize::from(ndigits));
        out.extend_from_slice(&ndigits.to_be_bytes());
        out.extend_from_slice(&weight.to_be_bytes());
        out.extend_from_slice(&sign.to_be_bytes());
        out.extend_from_slice(&dscale.to_be_bytes());
        out
    };
    if s.eq_ignore_ascii_case("nan") {
        return Some(header(0, 0, 0xC000, 0));
    }
    let (negative, unsigned) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int_part, frac_part) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if int_part.is_empty() && frac_part.is_empty()
        || !int_part
            .bytes()
            .chain(frac_part.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let dscale = u16::try_from(frac_part.len()).ok()?;
    // Pad so both parts split into groups of four digits around the decimal point.
    let int_pad = (4 - int_part.len() % 4) % 4;
    let frac_pad = (4 - frac_part.len() % 4) % 4;
    let digits: String = "0".repeat(int_pad) + int_part + frac_part + &"0".repeat(frac_pad);
    let int_groups = (int_part.len() + int_pad) / 4;
    let mut groups: Vec<i16> = digits
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).ok()?.parse::<i16>().ok())
        .collect::<Option<_>>()?;
    let mut weight = i16::try_from(int_groups).ok()? - 1;
    while groups.first() == Some(&0) {
        groups.remove(0);
        weight -= 1;
    }
    while groups.last() == Some(&0) {
        groups.pop();
    }
    if groups.is_empty() {
        return Some(header(0, 0, 0, dscale));
    }
    let sign = if negative { 0x4000 } else { 0 };
    let mut out = header(u16::try_from(groups.len()).ok()?, weight, sign, dscale);
    for g in groups {
        out.extend_from_slice(&g.to_be_bytes());
    }
    Some(out)
}

/// A decimal string from PostgreSQL binary `numeric`.
fn decode_numeric(bytes: &[u8]) -> PgWireResult<String> {
    let bad = || invalid("numeric", "truncated value");
    let word = |i: usize| -> PgWireResult<i16> {
        let b = bytes.get(i..i + 2).ok_or_else(bad)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    };
    let ndigits = usize::try_from(word(0)?).map_err(|_| bad())?;
    let weight = i64::from(word(2)?);
    let sign = word(4)? as u16;
    let dscale = usize::try_from(word(6)?).map_err(|_| bad())?;
    match sign {
        0xC000 => return Ok("NaN".to_owned()),
        0xD000 => return Ok("Infinity".to_owned()),
        0xF000 => return Ok("-Infinity".to_owned()),
        _ => {}
    }
    let digits: Vec<i64> = (0..ndigits)
        .map(|i| word(8 + 2 * i).map(i64::from))
        .collect::<PgWireResult<_>>()?;
    let group = |pos: i64| -> i64 {
        // Group at power 10000^pos is digits[weight - pos].
        usize::try_from(weight - pos)
            .ok()
            .and_then(|i| digits.get(i).copied())
            .unwrap_or(0)
    };
    let mut int_part = String::new();
    if weight >= 0 {
        for pos in (0..=weight).rev() {
            if int_part.is_empty() {
                int_part = group(pos).to_string();
            } else {
                int_part.push_str(&format!("{:04}", group(pos)));
            }
        }
    } else {
        int_part.push('0');
    }
    let mut frac = String::new();
    let mut pos = -1;
    while frac.len() < dscale {
        frac.push_str(&format!("{:04}", group(pos)));
        pos -= 1;
    }
    frac.truncate(dscale);
    let mut out = String::new();
    if sign == 0x4000 {
        out.push('-');
    }
    out.push_str(&int_part);
    if !frac.is_empty() {
        out.push('.');
        out.push_str(&frac);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_round_trip() {
        for s in [
            "0",
            "1",
            "-1",
            "12.34",
            "-0.001",
            "10000",
            "123456789.123456789",
            "0.50",
        ] {
            let bytes = encode_numeric(s).unwrap();
            assert_eq!(decode_numeric(&bytes).unwrap(), s, "{s}");
        }
    }

    #[test]
    fn numeric_known_encoding() {
        // 12.34: ndigits 2, weight 0, positive, dscale 2, digits [12, 3400]
        assert_eq!(
            encode_numeric("12.34").unwrap(),
            vec![0, 2, 0, 0, 0, 0, 0, 2, 0, 12, 0x0D, 0x48]
        );
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_timestamp("2000-01-01 00:00:00"), Some(0));
        assert_eq!(parse_timestamp("2000-01-01 00:00:01.5+00"), Some(1_500_000));
        assert_eq!(parse_timestamp("2000-01-01 01:00:00+01:00"), Some(0));
        assert_eq!(parse_timestamp("1999-12-31T23:00:00-01"), Some(0));
        assert_eq!(format_timestamp(1_500_000), "2000-01-01 00:00:01.5");
        assert_eq!(format_timestamp(-1), "1999-12-31 23:59:59.999999");
        let t = parse_timestamp("2024-02-29 12:34:56.123456").unwrap();
        assert_eq!(format_timestamp(t), "2024-02-29 12:34:56.123456");
    }

    #[test]
    fn dates_and_uuids() {
        assert_eq!(format_date(PG_EPOCH_DAYS), "2000-01-01");
        assert_eq!(parse_date("1970-01-01"), Some((0, "")));
        let u = "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11";
        assert_eq!(format_uuid(&parse_uuid(u).unwrap()), u);
    }
}
