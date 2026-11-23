//! Secret redaction shared by the commands that print outside text.

/// text with every occurrence of each secret replaced by one [redacted]. Overlapping or touching
/// matches merge first, so a secret that extends another leaves nothing visible.
pub fn redact_values<'a>(text: &str, secrets: impl Iterator<Item = &'a String>) -> String {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for secret in secrets {
        let mut from = 0;
        while let Some(at) = text[from..].find(secret.as_str()) {
            let start = from + at;
            spans.push((start, start + secret.len()));
            from = start + text[start..].chars().next().unwrap().len_utf8();
        }
    }
    spans.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    let (mut out, mut last) = (String::new(), 0);
    for (start, end) in merged {
        out.push_str(&text[last..start]);
        out.push_str("[redacted]");
        last = end;
    }
    out + &text[last..]
}
