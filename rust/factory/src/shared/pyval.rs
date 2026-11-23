//! Python's `==`, `str()`, `repr()` and truthiness on JSON values.

use serde_json::Value;

use super::py::repr;
use super::pyjson::{dumps, DEFAULT};

/// Python's `==` on two JSON values: bools and numbers compare as numbers, ints exactly, dicts by key set.
pub fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| py_eq(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        (Value::Bool(_) | Value::Number(_), Value::Bool(_) | Value::Number(_)) => {
            match (py_int(a), py_int(b)) {
                (Some(x), Some(y)) => x == y,
                _ => py_float(a) == py_float(b),
            }
        }
        _ => false,
    }
}

/// The digits of an int or bool, without a sign for zero, else None for a float.
pub fn py_int(v: &Value) -> Option<String> {
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

/// Python's `str()` of a JSON value, as an f-string prints it. A missing value prints as None.
pub fn py_str<'a>(v: impl Into<Option<&'a Value>>) -> String {
    match v.into() {
        Some(Value::String(s)) => s.clone(),
        Some(other) => py_repr(other),
        None => "None".into(),
    }
}

/// Python's `repr()` of a JSON value.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match n.to_string().parse::<f64>() {
            Ok(x) if x.is_infinite() => if x > 0.0 { "inf" } else { "-inf" }.into(),
            _ => py_int(v).unwrap_or_else(|| dumps(v, DEFAULT)),
        },
        Value::String(s) => repr(s),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => {
            let items: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", repr(k), py_repr(v)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

/// Python truthiness of a value that may be missing.
pub fn truthy<'a>(v: impl Into<Option<&'a Value>>) -> bool {
    match v.into() {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
    }
}
