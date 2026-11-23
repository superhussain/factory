//! Python's text rules: `repr`, `isprintable`, `isspace`, `strip`, `splitlines`, `round`.

use std::path::Path;

use super::unicode_printable::NON_PRINTABLE;

/// `str.__repr__`.
pub fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c if (c as u32) <= 0xff => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable` for one character.
pub fn is_printable(c: char) -> bool {
    let cp = c as u32;
    NON_PRINTABLE
        .binary_search_by(|&(lo, hi)| {
            if hi < cp {
                std::cmp::Ordering::Less
            } else if lo > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_err()
}

/// text on one line: non-printable characters become spaces, whitespace collapses, cut to limit chars.
pub fn printable_line(text: &str, limit: usize) -> String {
    let mapped: String = text
        .chars()
        .map(|c| if is_printable(c) { c } else { ' ' })
        .collect();
    mapped
        .split(' ')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

/// `str.isspace` for one character.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `str.splitlines(keepends)`.
pub fn splitlines(s: &str, keepends: bool) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let end = match c {
            '\r' => match chars.peek() {
                Some(&(j, '\n')) => {
                    chars.next();
                    j + 1
                }
                _ => i + 1,
            },
            '\n' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => i + c.len_utf8(),
            _ => continue,
        };
        lines.push(if keepends {
            &s[start..end]
        } else {
            &s[start..i]
        });
        start = end;
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

/// Python text mode's newline translation: `\r\n`, then a lone `\r`, become `\n`.
pub fn universal_newlines(s: String) -> String {
    if !s.contains('\r') {
        return s;
    }
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// `str(OSError)`: `[Errno n] message`, plus `: 'path'` when a path is given.
pub fn os_error(e: &std::io::Error, path: Option<&Path>) -> String {
    let text = e.to_string();
    let base = match e.raw_os_error() {
        Some(n) => {
            let suffix = format!(" (os error {n})");
            format!(
                "[Errno {n}] {}",
                text.strip_suffix(&suffix).unwrap_or(&text)
            )
        }
        None => text,
    };
    match path {
        Some(p) => format!("{base}: {}", repr(&p.to_string_lossy())),
        None => base,
    }
}

/// `round(x, ndigits)`.
pub fn round_half_even(x: f64, ndigits: u32) -> f64 {
    let n = ndigits as usize;
    format!("{x:.n$}").parse().unwrap()
}

/// `round(x)`.
pub fn round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr("it's"), "\"it's\"");
        assert_eq!(repr("a\tb\x00\u{200b}é"), "'a\\tb\\x00\\u200bé'");
        assert_eq!(repr("x\"y'z"), "'x\"y\\'z'");
        assert_eq!(repr("bad/x/y"), "'bad/x/y'");
        assert_eq!(
            repr("\u{7f}\u{a0}\u{e0001}\\"),
            "'\\x7f\\xa0\\U000e0001\\\\'"
        );
    }

    #[test]
    fn printable_matches_python() {
        assert!(is_printable('a') && is_printable(' ') && is_printable('é') && is_printable('😀'));
        assert!(
            !is_printable('\n')
                && !is_printable('\u{7f}')
                && !is_printable('\u{a0}')
                && !is_printable('\u{200b}')
        );
        assert_eq!(printable_line("  a\t\tb\u{3000}c\n", 200), "a b c");
        assert_eq!(printable_line("ééé", 2), "éé");
    }

    #[test]
    fn strip_and_splitlines_match_python() {
        assert_eq!(strip("\u{1c} a \u{85}"), "a");
        assert_eq!(lstrip(" a "), "a ");
        assert_eq!(rstrip(" a "), " a");
        assert_eq!(
            splitlines("a\r\nb\rc\u{2028}d\n", false),
            vec!["a", "b", "c", "d"]
        );
        assert_eq!(splitlines("a\r\nb\x0bc", true), vec!["a\r\n", "b\x0b", "c"]);
        assert_eq!(splitlines("", false), Vec::<&str>::new());
        assert_eq!(universal_newlines("a\r\nb\rc".into()), "a\nb\nc");
    }

    #[test]
    fn os_error_matches_python() {
        let e = std::io::Error::from_raw_os_error(2);
        assert_eq!(
            os_error(&e, Some(Path::new("/x"))),
            "[Errno 2] No such file or directory: '/x'"
        );
    }

    #[test]
    fn round_matches_python() {
        assert_eq!(round_int(2.5), 2);
        assert_eq!(round_int(-0.5), 0);
        assert_eq!(round_int(1.5), 2);
        assert_eq!(round_half_even(0.125, 2), 0.12);
        assert_eq!(round_half_even(2.675, 2), 2.67);
        assert_eq!(round_half_even(-2.5, 0), -2.0);
        assert_eq!(round_half_even(0.5, 1), 0.5);
    }
}
