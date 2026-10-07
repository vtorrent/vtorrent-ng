//! VTR payment URIs (BIP21-style) for payment requests and receive QR codes.
//!
//! Format: `vtorrent:<address>?amount=<VTR>&label=<name>&message=<note>&req=<id>`
//!
//! Amounts are parsed and formatted with **exact integer satoshi math** — never
//! floats. See `docs/payment-requests-design.md`.

use crate::address::validate_p2pkh;
use crate::error::{CoreError, Result};

/// The URI scheme prefix.
pub const SCHEME: &str = "vtorrent:";
const COIN: u64 = 100_000_000;

/// A parsed or to-be-built payment request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentUri {
    pub address: String,
    /// Amount in satoshis, if specified.
    pub amount_sats: Option<u64>,
    pub label: Option<String>,
    pub message: Option<String>,
    /// Optional request id (ties the payment to a stored request).
    pub req: Option<String>,
}

impl PaymentUri {
    /// Build a URI string. The address is validated.
    pub fn build(&self) -> Result<String> {
        validate_p2pkh(&self.address)?;
        let mut s = format!("{SCHEME}{}", self.address);
        let mut params: Vec<String> = Vec::new();
        if let Some(a) = self.amount_sats {
            params.push(format!("amount={}", format_sats(a)));
        }
        if let Some(l) = &self.label {
            params.push(format!("label={}", urlencode(l)));
        }
        if let Some(m) = &self.message {
            params.push(format!("message={}", urlencode(m)));
        }
        if let Some(r) = &self.req {
            params.push(format!("req={}", urlencode(r)));
        }
        if !params.is_empty() {
            s.push('?');
            s.push_str(&params.join("&"));
        }
        Ok(s)
    }

    /// Parse a URI string. Rejects a foreign scheme or an invalid address.
    pub fn parse(uri: &str) -> Result<Self> {
        let rest = uri
            .strip_prefix(SCHEME)
            .ok_or_else(|| CoreError::InvalidAddress(format!("not a {SCHEME} URI")))?;
        let (addr, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q)),
            None => (rest, None),
        };
        validate_p2pkh(addr)?;
        let mut out = PaymentUri {
            address: addr.to_string(),
            amount_sats: None,
            label: None,
            message: None,
            req: None,
        };
        if let Some(q) = query {
            for pair in q.split('&') {
                if pair.is_empty() {
                    continue;
                }
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                let v = urldecode(v);
                match k {
                    "amount" => out.amount_sats = Some(parse_sats(&v)?),
                    "label" => out.label = Some(v),
                    "message" => out.message = Some(v),
                    "req" => out.req = Some(v),
                    _ => {} // ignore unknown params
                }
            }
        }
        Ok(out)
    }
}

/// Parse a decimal VTR amount (e.g. `1.5`, `0.00000001`) into satoshis exactly.
pub fn parse_sats(vtr: &str) -> Result<u64> {
    let vtr = vtr.trim();
    if vtr.is_empty() {
        return Err(CoreError::InvalidAddress("empty amount".into()));
    }
    let (int_part, frac_part) = match vtr.split_once('.') {
        Some((i, f)) => (i, f),
        None => (vtr, ""),
    };
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return Err(CoreError::InvalidAddress(format!("invalid amount: {vtr}")));
    }
    if frac_part.len() > 8 {
        return Err(CoreError::InvalidAddress(format!(
            "amount has more than 8 decimals: {vtr}"
        )));
    }
    let int: u64 = if int_part.is_empty() {
        0
    } else {
        int_part
            .parse()
            .map_err(|_| CoreError::InvalidAddress(format!("amount overflow: {vtr}")))?
    };
    // Right-pad the fraction to 8 digits.
    let mut frac = frac_part.to_string();
    while frac.len() < 8 {
        frac.push('0');
    }
    let frac: u64 = if frac.is_empty() {
        0
    } else {
        frac.parse()
            .map_err(|_| CoreError::InvalidAddress(format!("invalid fraction: {vtr}")))?
    };
    int.checked_mul(COIN)
        .and_then(|s| s.checked_add(frac))
        .ok_or_else(|| CoreError::InvalidAddress(format!("amount overflow: {vtr}")))
}

/// Format satoshis as a decimal VTR string with no trailing zeros beyond the
/// significant digits (e.g. `1.5`, `0.00000001`, `2`).
pub fn format_sats(sats: u64) -> String {
    let int = sats / COIN;
    let frac = sats % COIN;
    if frac == 0 {
        return int.to_string();
    }
    let mut f = format!("{frac:08}");
    while f.ends_with('0') {
        f.pop();
    }
    format!("{int}.{f}")
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: &str = "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k";

    #[test]
    fn amount_conversion_is_exact() {
        assert_eq!(parse_sats("1").unwrap(), 100_000_000);
        assert_eq!(parse_sats("1.5").unwrap(), 150_000_000);
        assert_eq!(parse_sats("0.00000001").unwrap(), 1);
        assert_eq!(parse_sats("123.45678901").unwrap(), 12_345_678_901);
        assert_eq!(parse_sats(".5").unwrap(), 50_000_000);
        assert!(parse_sats("1.123456789").is_err()); // >8 decimals
        assert!(parse_sats("abc").is_err());
        assert!(parse_sats("").is_err());
    }

    #[test]
    fn format_is_minimal() {
        assert_eq!(format_sats(100_000_000), "1");
        assert_eq!(format_sats(150_000_000), "1.5");
        assert_eq!(format_sats(1), "0.00000001");
        assert_eq!(format_sats(123_456_789), "1.23456789");
    }

    #[test]
    fn build_parse_roundtrip() {
        let uri = PaymentUri {
            address: ADDR.into(),
            amount_sats: Some(150_000_000),
            label: Some("Alice & Bob".into()),
            message: Some("rent for 10/26".into()),
            req: Some("req-1".into()),
        };
        let s = uri.build().unwrap();
        assert!(s.starts_with("vtorrent:"));
        let parsed = PaymentUri::parse(&s).unwrap();
        assert_eq!(parsed, uri);
    }

    #[test]
    fn rejects_foreign_scheme_and_bad_address() {
        assert!(PaymentUri::parse("bitcoin:abc").is_err());
        assert!(PaymentUri::parse("vtorrent:not-an-address").is_err());
    }

    #[test]
    fn unknown_params_ignored() {
        let s = format!("{SCHEME}{ADDR}?amount=1&foo=bar");
        let p = PaymentUri::parse(&s).unwrap();
        assert_eq!(p.amount_sats, Some(100_000_000));
    }
}
