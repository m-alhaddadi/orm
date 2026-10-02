//! Postgres `numeric` in its binary wire format, to and from decimal text.
//!
//! The format is a sign, a scale and base-10000 digits:
//!
//! ```text
//! ndigits: i16, weight: i16 (exponent of the first digit), sign: u16, dscale: u16,
//! digits: [i16; ndigits]   value = sum(digits[k] * 10000^(weight - k))
//! ```
//!
//! Going through text keeps every digit: the frontend turns it into its own exact
//! decimal type (Python's `decimal.Decimal`), so no precision is lost on the way.

const POS: u16 = 0x0000;
const NEG: u16 = 0x4000;
const NAN: u16 = 0xC000;
const PINF: u16 = 0xD000;
const NINF: u16 = 0xF000;

fn u16_at(raw: &[u8], i: usize) -> Result<u16, String> {
    raw.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]])).ok_or_else(|| "truncated numeric".to_owned())
}

/// Decimal text for a binary `numeric`: `-12.50`, `0.001`, `NaN`, `Infinity`.
pub fn decode(raw: &[u8]) -> Result<String, String> {
    let ndigits = u16_at(raw, 0)? as usize;
    let weight = u16_at(raw, 2)? as i16 as i32;
    let sign = u16_at(raw, 4)?;
    let dscale = u16_at(raw, 6)? as usize;
    match sign {
        NAN => return Ok("NaN".into()),
        PINF => return Ok("Infinity".into()),
        NINF => return Ok("-Infinity".into()),
        POS | NEG => {}
        other => return Err(format!("invalid numeric sign {other:#x}")),
    }
    let digits = (0..ndigits).map(|k| u16_at(raw, 8 + 2 * k)).collect::<Result<Vec<_>, _>>()?;
    // the digit whose exponent is `e`
    let digit = |e: i32| -> u16 {
        let k = weight - e;
        if k < 0 || k as usize >= ndigits {
            0
        } else {
            digits[k as usize]
        }
    };
    let mut out = String::new();
    if sign == NEG {
        out.push('-');
    }
    if weight < 0 {
        out.push('0');
    } else {
        out.push_str(&digit(weight).to_string());
        for e in (0..weight).rev() {
            out.push_str(&format!("{:04}", digit(e)));
        }
    }
    if dscale > 0 {
        out.push('.');
        let mut frac = String::with_capacity(dscale + 4);
        let mut e = -1;
        while frac.len() < dscale {
            frac.push_str(&format!("{:04}", digit(e)));
            e -= 1;
        }
        frac.truncate(dscale);
        out.push_str(&frac);
    }
    Ok(out)
}

/// Binary `numeric` for plain decimal text (`-12.50`; no exponent).
pub fn encode(text: &str, out: &mut bytes::BytesMut) -> Result<(), String> {
    use bytes::BufMut;
    let (neg, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    if int.is_empty() && frac.is_empty() || !int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
        return Err(format!("invalid decimal {text:?}"));
    }
    let dscale = frac.len();
    // base-10000 groups: the integer part from the right, the fraction from the left
    let int = int.trim_start_matches('0');
    let pad = (4 - int.len() % 4) % 4;
    let int_digits = format!("{}{int}", "0".repeat(pad));
    let frac_digits = format!("{frac}{}", "0".repeat((4 - frac.len() % 4) % 4));
    let group = |s: &str| -> Vec<u16> { s.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap().parse().unwrap()).collect() };
    let mut digits = group(&int_digits);
    let mut weight = digits.len() as i32 - 1;
    digits.extend(group(&frac_digits));
    while digits.first() == Some(&0) {
        digits.remove(0);
        weight -= 1;
    }
    while digits.last() == Some(&0) {
        digits.pop();
    }
    if digits.is_empty() {
        weight = 0;
    }
    let too_big = || format!("decimal {text:?} is out of range");
    out.put_u16(u16::try_from(digits.len()).map_err(|_| too_big())?);
    out.put_i16(i16::try_from(weight).map_err(|_| too_big())?);
    out.put_u16(if neg && !digits.is_empty() { NEG } else { POS });
    out.put_u16(u16::try_from(dscale).map_err(|_| too_big())?);
    for d in digits {
        out.put_u16(d);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(s: &str) -> String {
        let mut buf = bytes::BytesMut::new();
        encode(s, &mut buf).unwrap();
        decode(&buf).unwrap()
    }

    #[test]
    fn decimals_survive_the_wire_format() {
        for s in ["0", "1", "-1", "12.50", "0.001", "-0.5", "10000", "123456789.123456789", "0.00000001", "99999999999999999999999999999999.1"] {
            assert_eq!(round_trip(s), s);
        }
        assert_eq!(round_trip("007.10"), "7.10");
        assert_eq!(round_trip("-0.00"), "0.00");
        assert_eq!(round_trip(".5"), "0.5");
        assert!(encode("1e5", &mut bytes::BytesMut::new()).is_err());
    }

    #[test]
    fn decodes_what_postgres_sends() {
        // 1234.5678 :: numeric(10, 4): weight 0, digits [1234, 5678]
        let raw = [0, 2, 0, 0, 0, 0, 0, 4, 0x04, 0xD2, 0x16, 0x2E];
        assert_eq!(decode(&raw).unwrap(), "1234.5678");
        assert_eq!(decode(&[0, 0, 0, 0, 0xC0, 0, 0, 0]).unwrap(), "NaN");
    }
}
