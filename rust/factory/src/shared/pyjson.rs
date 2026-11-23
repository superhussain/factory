//! Python's `json.dumps` output styles and `repr(float)`.

use serde_json::Value;

#[derive(Clone, Copy)]
pub struct Style {
    pub indent: Option<usize>,
    pub sort_keys: bool,
    pub ensure_ascii: bool,
    /// `separators=(",", ":")`.
    pub compact: bool,
}

/// `json.dumps(x)`.
pub const DEFAULT: Style = Style {
    indent: None,
    sort_keys: false,
    ensure_ascii: true,
    compact: false,
};
/// `json.dumps(x, indent=2)`.
pub const INDENT2: Style = Style {
    indent: Some(2),
    ..DEFAULT
};
/// `json.dumps(x, indent=2, sort_keys=True)`.
pub const INDENT2_SORTED: Style = Style {
    indent: Some(2),
    sort_keys: true,
    ..DEFAULT
};
/// `json.dumps(x, sort_keys=True)`.
pub const SORTED: Style = Style {
    sort_keys: true,
    ..DEFAULT
};
/// `json.dumps(x, ensure_ascii=False)`.
pub const NO_ASCII: Style = Style {
    ensure_ascii: false,
    ..DEFAULT
};
/// `json.dumps(x, separators=(",", ":"))`.
pub const COMPACT: Style = Style {
    compact: true,
    ..DEFAULT
};

pub fn dumps(v: &Value, style: Style) -> String {
    let mut out = String::new();
    write_value(&mut out, v, style, 0);
    out
}

/// `json.loads`.
pub fn loads(text: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(text)
}

fn newline(out: &mut String, style: Style, level: usize) {
    if let Some(n) = style.indent {
        out.push('\n');
        out.push_str(&" ".repeat(n * level));
    }
}

fn item_sep(style: Style) -> &'static str {
    if style.compact || style.indent.is_some() {
        ","
    } else {
        ", "
    }
}

fn write_value(out: &mut String, v: &Value, style: Style, level: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number(&n.to_string())),
        Value::String(s) => write_str(out, s, style.ensure_ascii),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep(style));
                }
                newline(out, style, level + 1);
                write_value(out, item, style, level + 1);
            }
            newline(out, style, level);
            out.push(']');
        }
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if style.sort_keys {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            out.push('{');
            for (i, (key, value)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep(style));
                }
                newline(out, style, level + 1);
                write_str(out, key, style.ensure_ascii);
                out.push_str(if style.compact { ":" } else { ": " });
                write_value(out, value, style, level + 1);
            }
            newline(out, style, level);
            out.push('}');
        }
    }
}

/// A JSON number's text as Python prints the int or float it parses to.
fn number(text: &str) -> String {
    if text.contains(['.', 'e', 'E']) {
        return float_repr(text.parse().unwrap());
    }
    if text == "-0" {
        return "0".into();
    }
    text.into()
}

fn write_str(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || ensure_ascii && (c as u32) >= 0x7f => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Python's `repr(float)`.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let mut sci = format!("{x:e}");
    // At 17 digits the shortest form can break a tie upward; Python rounds the exact value half to even.
    if sci
        .split('e')
        .next()
        .unwrap()
        .chars()
        .filter(char::is_ascii_digit)
        .count()
        == 17
    {
        sci = format!("{x:.16e}");
    }
    let (mantissa, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let body = if (-4..16).contains(&exp) {
        if exp < 0 {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        } else {
            let point = exp as usize + 1;
            if digits.len() <= point {
                format!("{digits}{}.0", "0".repeat(point - digits.len()))
            } else {
                format!("{}.{}", &digits[..point], &digits[point..])
            }
        }
    } else {
        let tail = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        format!("{tail}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLES: [Style; 6] = [DEFAULT, INDENT2, INDENT2_SORTED, SORTED, NO_ASCII, COMPACT];

    fn all(text: &str) -> Vec<String> {
        let v = loads(text).unwrap();
        STYLES.iter().map(|s| dumps(&v, *s)).collect()
    }

    #[test]
    fn floats_match_python_repr() {
        for (text, want) in [
            ("1e16", "1e+16"),
            ("1e15", "1000000000000000.0"),
            ("1e-05", "1e-05"),
            ("0.0001", "0.0001"),
            ("-0.0", "-0.0"),
            ("2.5e-07", "2.5e-07"),
            ("0.1", "0.1"),
            ("1.5e300", "1.5e+300"),
            ("123.456", "123.456"),
            ("1e400", "Infinity"),
            ("-1e400", "-Infinity"),
            ("1059438285926254.25", "1059438285926254.2"),
        ] {
            assert_eq!(all(text), vec![want; 6], "{text}");
        }
        assert_eq!(float_repr(f64::NAN), "NaN");
    }

    #[test]
    fn ints_keep_their_digits() {
        assert_eq!(
            all("123456789012345678901234567890"),
            vec!["123456789012345678901234567890"; 6]
        );
        assert_eq!(all("-0"), vec!["0"; 6]);
        assert_eq!(all("-7"), vec!["-7"; 6]);
    }

    #[test]
    fn strings_escape_as_python() {
        let got = all("\"café ✓ \\ud83d\\ude00 \\u007f\\u001f\"");
        let ascii = "\"caf\\u00e9 \\u2713 \\ud83d\\ude00 \\u007f\\u001f\"";
        let raw = "\"café ✓ 😀 \u{7f}\\u001f\"";
        assert_eq!(got, vec![ascii, ascii, ascii, ascii, raw, ascii]);
        assert_eq!(
            all("\"\\\"\\\\\\n\\r\\t\\b\\f\""),
            vec!["\"\\\"\\\\\\n\\r\\t\\b\\f\""; 6]
        );
    }

    #[test]
    fn containers_match_python() {
        assert_eq!(all("{}"), vec!["{}"; 6]);
        assert_eq!(all("[]"), vec!["[]"; 6]);
        assert_eq!(
            all(r#"{"b": [1, {}], "a": {"c": null, "d": true}}"#),
            vec![
                r#"{"b": [1, {}], "a": {"c": null, "d": true}}"#,
                "{\n  \"b\": [\n    1,\n    {}\n  ],\n  \"a\": {\n    \"c\": null,\n    \"d\": true\n  }\n}",
                "{\n  \"a\": {\n    \"c\": null,\n    \"d\": true\n  },\n  \"b\": [\n    1,\n    {}\n  ]\n}",
                r#"{"a": {"c": null, "d": true}, "b": [1, {}]}"#,
                r#"{"b": [1, {}], "a": {"c": null, "d": true}}"#,
                r#"{"b":[1,{}],"a":{"c":null,"d":true}}"#,
            ]
        );
    }
}
