//! A small, dependency-free JSON parser and printer used for value
//! interchange by the CLI, the C ABI and the bindings.
//!
//! JSON is an *API* model, never a storage format (see `docs/SPEC.md` §6).
//! Supported: null, true, false, numbers, strings (with escapes and \uXXXX),
//! arrays, objects. Binary values print as `"0x...."` hex strings; non-finite
//! floats print as `null`.

use std::collections::BTreeMap;

use crate::document::Value;

/// Parse a JSON document into a [`Value`].
pub fn parse(input: &str) -> Result<Value, String> {
    let mut p = Parser {
        s: input.as_bytes(),
        pos: 0,
    };
    let v = p.parse_value()?;
    p.skip_ws();
    if p.pos != p.s.len() {
        return Err(format!("trailing characters at position {}", p.pos));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while let Some(&b) = self.s.get(self.pos) {
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn err(&self, msg: &str) -> String {
        format!("{msg} at position {}", self.pos)
    }

    fn parse_value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.s.get(self.pos).copied() {
            Some(b'n') => {
                self.expect_lit(b"null")?;
                Ok(Value::Null)
            }
            Some(b't') => {
                self.expect_lit(b"true")?;
                Ok(Value::Bool(true))
            }
            Some(b'f') => {
                self.expect_lit(b"false")?;
                Ok(Value::Bool(false))
            }
            Some(b'"') => Ok(Value::Str(self.parse_string()?)),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            other => Err(self.err(&format!(
                "expected value, found {}",
                other
                    .map(|b| (b as char).to_string())
                    .unwrap_or_else(|| "EOF".to_string())
            ))),
        }
    }

    fn expect_lit(&mut self, lit: &[u8]) -> Result<(), String> {
        if self.s.len() - self.pos >= lit.len() && &self.s[self.pos..self.pos + lit.len()] == lit {
            self.pos += lit.len();
            Ok(())
        } else {
            Err(self.err(&format!(
                "expected literal {}",
                String::from_utf8_lossy(lit)
            )))
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        debug_assert_eq!(self.s.get(self.pos), Some(&b'"'));
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            let b = *self
                .s
                .get(self.pos)
                .ok_or_else(|| self.err("unterminated string"))?;
            self.pos += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let esc = *self
                        .s
                        .get(self.pos)
                        .ok_or_else(|| self.err("unterminated escape"))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let hi = self.parse_hex4()?;
                            let c = if (0xD800..=0xDBFF).contains(&hi) {
                                // High surrogate: expect a low surrogate.
                                if self.s.get(self.pos) == Some(&b'\\')
                                    && self.s.get(self.pos + 1) == Some(&b'u')
                                {
                                    self.pos += 2;
                                    let lo = self.parse_hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&lo) {
                                        return Err(self.err("invalid low surrogate"));
                                    }
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    return Err(self.err("unpaired high surrogate"));
                                }
                            } else if (0xDC00..=0xDFFF).contains(&hi) {
                                return Err(self.err("unpaired low surrogate"));
                            } else {
                                hi
                            };
                            let ch = char::from_u32(c).ok_or_else(|| self.err("invalid \\u"))?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        other => {
                            return Err(self.err(&format!("invalid escape \\{}", other as char)))
                        }
                    }
                }
                0x00..=0x1F => return Err(self.err("control character in string")),
                _ => out.push(b),
            }
        }
        String::from_utf8(out).map_err(|_| self.err("invalid UTF-8 in string"))
    }

    fn parse_hex4(&mut self) -> Result<u32, String> {
        if self.pos + 4 > self.s.len() {
            return Err(self.err("truncated \\u escape"));
        }
        let mut v = 0u32;
        for i in 0..4 {
            let b = self.s[self.pos + i];
            v = (v << 4) | hex_val(b).ok_or_else(|| self.err("invalid hex digit"))?;
        }
        self.pos += 4;
        Ok(v)
    }

    fn parse_number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        let raw_starts_minus = self.s.get(self.pos) == Some(&b'-');
        if raw_starts_minus {
            self.pos += 1;
        }
        let mut is_float = false;
        let mut digits = 0usize;
        while let Some(&b) = self.s.get(self.pos) {
            if b.is_ascii_digit() {
                digits += 1;
                self.pos += 1;
            } else {
                break;
            }
        }
        if digits == 0 {
            return Err(self.err("invalid number"));
        }
        // Strict JSON: no leading zeros ("01" is invalid).
        let first_digit = if raw_starts_minus {
            self.s[start + 1]
        } else {
            self.s[start]
        };
        if first_digit == b'0' && digits > 1 {
            return Err(self.err("invalid number: leading zeros"));
        }
        if self.s.get(self.pos) == Some(&b'.') {
            is_float = true;
            self.pos += 1;
            let mut frac_digits = 0usize;
            while let Some(&b) = self.s.get(self.pos) {
                if b.is_ascii_digit() {
                    frac_digits += 1;
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if frac_digits == 0 {
                return Err(self.err("invalid number: expected digits after '.'"));
            }
        }
        if matches!(self.s.get(self.pos), Some(&b'e') | Some(&b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.s.get(self.pos), Some(&b'+') | Some(&b'-')) {
                self.pos += 1;
            }
            let mut exp_digits = 0usize;
            while let Some(&b) = self.s.get(self.pos) {
                if b.is_ascii_digit() {
                    exp_digits += 1;
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if exp_digits == 0 {
                return Err(self.err("invalid number: expected exponent digits"));
            }
        }
        let raw = &self.s[start..self.pos];
        if !is_float {
            // Try exact integer, fall back to float on overflow.
            if raw.starts_with(b"-") {
                if let Ok(v) = parse_i64(raw) {
                    return Ok(Value::Int(v));
                }
            } else if let Ok(v) = parse_u64(raw) {
                if v <= i64::MAX as u64 {
                    return Ok(Value::Int(v as i64));
                }
            }
        }
        let text = std::str::from_utf8(raw).map_err(|_| self.err("invalid number"))?;
        let f: f64 = text.parse().map_err(|_| self.err("invalid number"))?;
        Ok(Value::Float(f))
    }

    fn parse_array(&mut self) -> Result<Value, String> {
        debug_assert_eq!(self.s.get(self.pos), Some(&b'['));
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.s.get(self.pos) == Some(&b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.s.get(self.pos).copied() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.err("expected ',' or ']' in array")),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Value, String> {
        debug_assert_eq!(self.s.get(self.pos), Some(&b'{'));
        self.pos += 1;
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.s.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.skip_ws();
            if self.s.get(self.pos) != Some(&b'"') {
                return Err(self.err("expected string key in object"));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.s.get(self.pos) != Some(&b':') {
                return Err(self.err("expected ':' after object key"));
            }
            self.pos += 1;
            let value = self.parse_value()?;
            map.insert(key, value);
            self.skip_ws();
            match self.s.get(self.pos).copied() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(map));
                }
                _ => return Err(self.err("expected ',' or '}' in object")),
            }
        }
    }
}

fn hex_val(b: u8) -> Option<u32> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as u32),
        b'a'..=b'f' => Some((b - b'a' + 10) as u32),
        b'A'..=b'F' => Some((b - b'A' + 10) as u32),
        _ => None,
    }
}

fn parse_i64(raw: &[u8]) -> Result<i64, ()> {
    let mut v: i64 = 0;
    for &b in &raw[1..] {
        v = v
            .checked_mul(10)
            .ok_or(())?
            .checked_sub((b - b'0') as i64)
            .ok_or(())?;
    }
    Ok(v)
}

fn parse_u64(raw: &[u8]) -> Result<u64, ()> {
    let mut v: u64 = 0;
    for &b in raw {
        v = v
            .checked_mul(10)
            .ok_or(())?
            .checked_add((b - b'0') as u64)
            .ok_or(())?;
    }
    Ok(v)
}

/// Print a value as compact JSON.
#[allow(dead_code)] // used by tests
pub fn stringify(v: &Value) -> String {
    let mut out = String::new();
    write_value(v, &mut out);
    out
}

/// Print a value as pretty JSON with 2-space indentation.
pub fn stringify_pretty(v: &Value) -> String {
    let mut out = String::new();
    write_value_indented(v, &mut out, 0);
    out
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_value(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => write_float(*f, out),
        Value::Str(s) => write_string(out, s),
        Value::Binary(b) => {
            out.push('"');
            out.push_str("0x");
            for byte in b {
                out.push_str(&format!("{byte:02x}"));
            }
            out.push('"');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, k);
                out.push(':');
                write_value(val, out);
            }
            out.push('}');
        }
    }
}

fn write_float(f: f64, out: &mut String) {
    if !f.is_finite() {
        // JSON has no NaN/Infinity; emit null like serde_json's default.
        out.push_str("null");
        return;
    }
    if f.fract() == 0.0 && f.abs() < 1e16 {
        out.push_str(&format!("{f:.1}"));
    } else {
        out.push_str(&format!("{f}"));
    }
}

fn write_value_indented(v: &Value, out: &mut String, depth: usize) {
    match v {
        Value::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                indent(out, depth + 1);
                write_value_indented(item, out, depth + 1);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, depth);
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push_str("{\n");
            for (i, (k, val)) in map.iter().enumerate() {
                indent(out, depth + 1);
                write_string(out, k);
                out.push_str(": ");
                write_value_indented(val, out, depth + 1);
                if i + 1 < map.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, depth);
            out.push('}');
        }
        _ => write_value(v, out),
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(input: &str) -> Value {
        let v = parse(input).unwrap();
        let s = stringify(&v);
        let v2 = parse(&s).unwrap();
        assert_eq!(v, v2, "roundtrip of {input:?} produced {s:?}");
        v
    }

    #[test]
    fn scalars() {
        assert_eq!(rt("null"), Value::Null);
        assert_eq!(rt("true"), Value::Bool(true));
        assert_eq!(rt("false"), Value::Bool(false));
        assert_eq!(rt("42"), Value::Int(42));
        assert_eq!(rt("-17"), Value::Int(-17));
        assert_eq!(rt("3.25"), Value::Float(3.25));
        assert_eq!(rt("1e3"), Value::Float(1000.0));
        assert_eq!(rt("\"héllo\""), Value::Str("héllo".to_string()));
    }

    #[test]
    fn structures() {
        let v = rt("[1, 2.5, \"x\", null, true]");
        assert_eq!(
            v,
            Value::Array(vec![
                Value::Int(1),
                Value::Float(2.5),
                Value::Str("x".to_string()),
                Value::Null,
                Value::Bool(true),
            ])
        );
        let v = rt("{\"a\": {\"b\": [1]}, \"c\": {}}");
        assert!(v.as_object().is_some());
    }

    #[test]
    fn escapes() {
        assert_eq!(
            rt("\"a\\nb\\t\\\"q\\\\\""),
            Value::Str("a\nb\t\"q\\".to_string())
        );
        assert_eq!(rt("\"\\u0041\\u00e9\""), Value::Str("Aé".to_string()));
        assert_eq!(rt("\"\\ud83d\\ude00\""), Value::Str("😀".to_string()));
    }

    #[test]
    fn errors() {
        assert!(parse("").is_err());
        assert!(parse("{").is_err());
        assert!(parse("[1,]").is_err());
        assert!(parse("01").is_err());
        assert!(parse("\"\\uZZZZ\"").is_err());
        assert!(parse("nul").is_err());
        assert!(parse("1 2").is_err());
    }

    #[test]
    fn float_output_style() {
        assert_eq!(stringify(&Value::Float(1.0)), "1.0");
        assert_eq!(stringify(&Value::Float(2.5)), "2.5");
        assert_eq!(stringify(&Value::Float(f64::NAN)), "null");
    }

    #[test]
    fn binary_prints_as_hex() {
        assert_eq!(
            stringify(&Value::Binary(vec![0xDE, 0xAD, 0xBE, 0xEF])),
            "\"0xdeadbeef\""
        );
    }
}
