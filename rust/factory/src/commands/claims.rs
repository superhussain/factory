//! claim and board, plus the claim helpers hold, handoff and other slices share. hold and handoff wait for the
//! sync slice.

use serde_json::Value;

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;
use crate::shared::py::repr;
use crate::shared::pyjson::{dumps, DEFAULT};

pub fn claim(_call: &Call) -> Res<()> {
    Err(not_ported("claim"))
}

pub fn board(_call: &Call) -> Res<()> {
    Err(not_ported("board"))
}

pub fn hold(_call: &Call) -> Res<()> {
    Err(not_ported("hold"))
}

pub fn handoff(_call: &Call) -> Res<()> {
    Err(not_ported("handoff"))
}

/// Python's `==` on two JSON values: bools and numbers compare as numbers, ints exactly, dicts by key set.
fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| py_eq(x, y)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        (Value::Bool(_) | Value::Number(_), Value::Bool(_) | Value::Number(_)) => match (py_int(a), py_int(b)) {
            (Some(x), Some(y)) => x == y,
            _ => py_float(a) == py_float(b),
        },
        _ => false,
    }
}

/// The digits of an int or bool, without a sign for zero, else None for a float.
fn py_int(v: &Value) -> Option<String> {
    let text = match v {
        Value::Bool(b) => return Some(u8::from(*b).to_string()),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if text.contains(['.', 'e', 'E']) {
        return None;
    }
    Some(if text == "-0" { "0".into() } else { text })
}

fn py_float(v: &Value) -> f64 {
    match v {
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// Python's `str()` of a JSON value, as an f-string prints it.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python's `repr()` of a JSON value.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(_) => dumps(v, DEFAULT),
        Value::String(s) => repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => {
            let items: Vec<String> = map.iter().map(|(k, v)| format!("{}: {}", repr(k), py_repr(v))).collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

/// Python truthiness of a value that may be missing.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
    }
}

type Scan = Result<usize, (&'static str, usize)>;

fn skip_ws(c: &[char], mut idx: usize) -> usize {
    while idx < c.len() && matches!(c[idx], ' ' | '\t' | '\n' | '\r') {
        idx += 1;
    }
    idx
}

/// The `json.JSONDecodeError` text CPython's C scanner raises for s, or "" when it would parse s.
/// Call it only after `pyjson::loads` fails.
fn py_json_error(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let found = if c.first() == Some(&'\u{feff}') {
        Err(("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0))
    } else {
        scan_value(&c, skip_ws(&c, 0)).and_then(|end| {
            let end = skip_ws(&c, end);
            if end == c.len() { Ok(end) } else { Err(("Extra data", end)) }
        })
    };
    let Err((msg, pos)) = found else { return String::new() };
    let lineno = c[..pos].iter().filter(|&&x| x == '\n').count() + 1;
    let colno = match c[..pos].iter().rposition(|&x| x == '\n') {
        Some(nl) => pos - nl,
        None => pos + 1,
    };
    format!("{msg}: line {lineno} column {colno} (char {pos})")
}

fn scan_value(c: &[char], idx: usize) -> Scan {
    let Some(&first) = c.get(idx) else { return Err(("Expecting value", idx)) };
    let rest = &c[idx..];
    let word = |w: &str| rest.len() >= w.chars().count() && rest.iter().zip(w.chars()).all(|(a, b)| *a == b);
    match first {
        '"' => scan_string(c, idx + 1),
        '{' => scan_object(c, idx + 1),
        '[' => scan_array(c, idx + 1),
        'n' if word("null") => Ok(idx + 4),
        't' if word("true") => Ok(idx + 4),
        'f' if word("false") => Ok(idx + 5),
        'N' if word("NaN") => Ok(idx + 3),
        'I' if word("Infinity") => Ok(idx + 8),
        '-' if word("-Infinity") => Ok(idx + 9),
        _ => scan_number(c, idx),
    }
}

fn scan_number(c: &[char], start: usize) -> Scan {
    let digit = |i: usize| c.get(i).is_some_and(char::is_ascii_digit);
    let len = c.len();
    let mut idx = start;
    if c[idx] == '-' {
        idx += 1;
        if idx >= len {
            return Err(("Expecting value", start));
        }
    }
    if ('1'..='9').contains(&c[idx]) {
        idx += 1;
        while digit(idx) {
            idx += 1;
        }
    } else if c[idx] == '0' {
        idx += 1;
    } else {
        return Err(("Expecting value", start));
    }
    if idx + 1 < len && c[idx] == '.' && digit(idx + 1) {
        idx += 2;
        while digit(idx) {
            idx += 1;
        }
    }
    if idx + 1 < len && matches!(c[idx], 'e' | 'E') {
        let e_start = idx;
        idx += 1;
        if idx + 1 < len && matches!(c[idx], '-' | '+') {
            idx += 1;
        }
        while digit(idx) {
            idx += 1;
        }
        if !digit(idx - 1) {
            idx = e_start;
        }
    }
    Ok(idx)
}

fn scan_string(c: &[char], mut end: usize) -> Scan {
    let begin = end - 1;
    let len = c.len();
    let hex = |range: std::ops::Range<usize>| c[range].iter().try_fold(0u32, |n, d| Some(n * 16 + d.to_digit(16)?));
    loop {
        let mut next = end;
        while next < len && !matches!(c[next], '"' | '\\') {
            if (c[next] as u32) <= 0x1f {
                return Err(("Invalid control character at", next));
            }
            next += 1;
        }
        if next == len {
            return Err(("Unterminated string starting at", begin));
        }
        let quote = c[next] == '"';
        next += 1;
        if quote {
            return Ok(next);
        }
        if next == len {
            return Err(("Unterminated string starting at", begin));
        }
        if c[next] != 'u' {
            end = next + 1;
            if !matches!(c[next], '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') {
                return Err(("Invalid \\escape", end - 2));
            }
            continue;
        }
        next += 1;
        end = next + 4;
        if end >= len {
            return Err(("Invalid \\uXXXX escape", next - 1));
        }
        let Some(high) = hex(next..end) else { return Err(("Invalid \\uXXXX escape", end - 5)) };
        if (0xd800..0xdc00).contains(&high) && end + 6 < len && c[end] == '\\' && c[end + 1] == 'u' {
            end += 6;
            let Some(low) = hex(end - 4..end) else { return Err(("Invalid \\uXXXX escape", end - 5)) };
            if !(0xdc00..0xe000).contains(&low) {
                end -= 6;
            }
        }
    }
}

fn scan_object(c: &[char], idx: usize) -> Scan {
    let mut idx = skip_ws(c, idx);
    if c.get(idx) != Some(&'}') {
        loop {
            if c.get(idx) != Some(&'"') {
                return Err(("Expecting property name enclosed in double quotes", idx));
            }
            idx = skip_ws(c, scan_string(c, idx + 1)?);
            if c.get(idx) != Some(&':') {
                return Err(("Expecting ':' delimiter", idx));
            }
            idx = skip_ws(c, scan_value(c, skip_ws(c, idx + 1))?);
            if c.get(idx) == Some(&'}') {
                break;
            }
            if c.get(idx) != Some(&',') {
                return Err(("Expecting ',' delimiter", idx));
            }
            idx = skip_ws(c, idx + 1);
        }
    }
    Ok(idx + 1)
}

fn scan_array(c: &[char], idx: usize) -> Scan {
    let mut idx = skip_ws(c, idx);
    if c.get(idx) != Some(&']') {
        loop {
            idx = skip_ws(c, scan_value(c, idx)?);
            if c.get(idx) == Some(&']') {
                break;
            }
            if c.get(idx) != Some(&',') {
                return Err(("Expecting ',' delimiter", idx));
            }
            idx = skip_ws(c, idx + 1);
        }
    }
    Ok(idx + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn py_json_error_matches_cpython() {
        for (text, want) in [
            ("nope", "Expecting value: line 1 column 1 (char 0)"),
            ("{\"a\" 1}", "Expecting ':' delimiter: line 1 column 6 (char 5)"),
            ("{} x", "Extra data: line 1 column 4 (char 3)"),
            ("\"\\q\"", "Invalid \\escape: line 1 column 2 (char 1)"),
            ("\"a\tb\"", "Invalid control character at: line 1 column 3 (char 2)"),
            ("\n\n  [é", "Expecting value: line 3 column 4 (char 5)"),
            ("[1, 2", "Expecting ',' delimiter: line 1 column 6 (char 5)"),
            ("{\"a\": 1,}", "Expecting property name enclosed in double quotes: line 1 column 9 (char 8)"),
            ("\"abc", "Unterminated string starting at: line 1 column 1 (char 0)"),
            ("\u{feff}{}", "Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)"),
            ("[", "Expecting value: line 1 column 2 (char 1)"),
            ("[1,]", "Expecting value: line 1 column 4 (char 3)"),
            ("1.e5", "Extra data: line 1 column 2 (char 1)"),
            ("\"\\u12\"", "Invalid \\uXXXX escape: line 1 column 3 (char 2)"),
            ("\"\\ud800\\uzzzz\"", "Invalid \\uXXXX escape: line 1 column 9 (char 8)"),
            ("[NaN, -Infinity, 1.5e3, \"\\ud800\\u0041\"]", ""),
        ] {
            assert_eq!(py_json_error(text), want, "{text:?}");
        }
    }

    #[test]
    fn py_eq_follows_python() {
        assert!(py_eq(&json!(1), &json!(true)) && py_eq(&json!(0.0), &json!(false)) && py_eq(&json!(1), &json!(1.0)));
        assert!(!py_eq(&json!(2), &json!(true)) && !py_eq(&json!("1"), &json!(1)) && !py_eq(&json!(null), &json!(false)));
        assert!(py_eq(&json!({"a": [1, {"b": null}]}), &json!({"a": [true, {"b": null}]})));
        assert!(!py_eq(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
        let big = serde_json::from_str::<Value>("123456789012345678901234567890").unwrap();
        assert!(py_eq(&big, &big.clone()) && !py_eq(&big, &serde_json::from_str("123456789012345678901234567891").unwrap()));
    }

    #[test]
    fn py_str_and_truthy_follow_python() {
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!("x")), "x");
        assert_eq!(py_str(&json!(1.0)), "1.0");
        assert_eq!(py_str(&json!({"a": [true, "it's", null]})), r#"{'a': [True, "it's", None]}"#);
        assert!(!truthy(None) && !truthy(Some(&json!(0))) && !truthy(Some(&json!(""))) && !truthy(Some(&json!({}))));
        assert!(truthy(Some(&json!(0.5))) && truthy(Some(&json!([0]))));
    }
}
