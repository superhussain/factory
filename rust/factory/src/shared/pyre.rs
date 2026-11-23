//! Python `re` patterns on fancy-regex: `translate` rewrites the parts whose meaning differs.

use std::sync::OnceLock;

use fancy_regex::Regex;

pub type Captures<'t> = fancy_regex::Captures<'t, str>;

#[derive(Clone, Copy)]
pub struct Flags {
    pub ascii: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dotall: bool,
}

impl Flags {
    pub const NONE: Flags = Flags {
        ascii: false,
        ignore_case: false,
        multiline: false,
        dotall: false,
    };
    pub const ASCII: Flags = Flags {
        ascii: true,
        ..Flags::NONE
    };
    pub const I: Flags = Flags {
        ignore_case: true,
        ..Flags::NONE
    };
    pub const M: Flags = Flags {
        multiline: true,
        ..Flags::NONE
    };
    pub const S: Flags = Flags {
        dotall: true,
        ..Flags::NONE
    };
}

/// A Python pattern, compiled on first use.
pub struct Pattern {
    src: &'static str,
    flags: Flags,
    search: OnceLock<Regex>,
    start: OnceLock<Regex>,
    full: OnceLock<Regex>,
}

const BACKTRACK: &str = "regex backtrack limit";

impl Pattern {
    pub const fn new(src: &'static str, flags: Flags) -> Pattern {
        Pattern {
            src,
            flags,
            search: OnceLock::new(),
            start: OnceLock::new(),
            full: OnceLock::new(),
        }
    }

    fn compiled(&self, cell: &'static str) -> &Regex {
        let (lock, before, after) = match cell {
            "start" => (&self.start, "\\A(?:", ")"),
            "full" => (&self.full, "\\A(?:", ")\\z"),
            _ => (&self.search, "", ""),
        };
        lock.get_or_init(|| {
            let source = format!(
                "{}{before}{}{after}",
                prefix(self.flags),
                body(self.src, self.flags)
            );
            Regex::new(&source).unwrap_or_else(|e| panic!("bad pattern {:?}: {e}", self.src))
        })
    }

    /// `re.search`.
    pub fn search<'t>(&self, s: &'t str) -> Option<Captures<'t>> {
        self.compiled("search").captures(s).expect(BACKTRACK)
    }

    /// `re.match`.
    pub fn match_start<'t>(&self, s: &'t str) -> Option<Captures<'t>> {
        self.compiled("start").captures(s).expect(BACKTRACK)
    }

    /// `re.fullmatch`.
    pub fn fullmatch<'t>(&self, s: &'t str) -> Option<Captures<'t>> {
        self.compiled("full").captures(s).expect(BACKTRACK)
    }

    pub fn is_full(&self, s: &str) -> bool {
        self.compiled("full").is_match(s).expect(BACKTRACK)
    }

    /// `re.finditer`, as matches.
    pub fn find_iter<'r, 't>(
        &'r self,
        s: &'t str,
    ) -> impl Iterator<Item = fancy_regex::Match<'t>> + use<'r, 't> {
        self.compiled("search")
            .find_iter(s)
            .map(|m| m.expect(BACKTRACK))
    }

    /// `re.sub` with a function.
    pub fn replace_all(&self, s: &str, f: impl FnMut(&Captures) -> String) -> String {
        self.compiled("search")
            .try_replacen(s, 0, f)
            .expect(BACKTRACK)
            .into_owned()
    }
}

fn prefix(flags: Flags) -> String {
    let mut out = String::new();
    // ASCII ignore-case folds letters in body() instead, since (?i) also folds Unicode look-alikes.
    let unicode_case = flags.ignore_case && !flags.ascii;
    for (on, flag) in [
        (unicode_case, "(?i)"),
        (flags.multiline, "(?m)"),
        (flags.dotall, "(?s)"),
    ] {
        if on {
            out.push_str(flag);
        }
    }
    out
}

/// A Python pattern as a fancy-regex pattern with the same meaning, flags included.
pub fn translate(src: &str, flags: Flags) -> String {
    prefix(flags) + &body(src, flags)
}

fn body(src: &str, flags: Flags) -> String {
    let word = if flags.ascii {
        "0-9A-Za-z_"
    } else {
        "\\p{L}\\p{N}_"
    };
    let space = if flags.ascii {
        "\\t\\n\\x0B\\x0C\\r "
    } else {
        "\\s\\x1C-\\x1F"
    };
    let digit = if flags.ascii { "0-9" } else { "\\d" };
    let w = format!("[{word}]");
    let fold = flags.ascii && flags.ignore_case;
    let swap = |c: char| {
        if c.is_ascii_lowercase() {
            c.to_ascii_uppercase()
        } else {
            c.to_ascii_lowercase()
        }
    };
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_class = false;
    while let Some(c) = chars.next() {
        if in_class {
            match c {
                ']' => {
                    in_class = false;
                    out.push(']');
                }
                '[' => out.push_str("\\["),
                '\\' => {
                    let e = chars.next().expect("trailing backslash");
                    match e {
                        'w' => out.push_str(word),
                        'd' => out.push_str(digit),
                        's' => out.push_str(space),
                        'W' | 'D' | 'S' => panic!("\\{e} unsupported in class: {src:?}"),
                        _ => {
                            out.push('\\');
                            out.push(e);
                            match copy_escape_tail(e, &mut chars, &mut out) {
                                Some(c) if fold && c.is_ascii_alphabetic() => out.push(swap(c)),
                                _ => {}
                            }
                        }
                    }
                }
                c if fold && c.is_ascii_alphabetic() => {
                    let mut ahead = chars.clone();
                    match (ahead.next(), ahead.next()) {
                        (Some('-'), Some(d)) if d.is_ascii_alphabetic() => {
                            chars.next();
                            chars.next();
                            out.extend([c, '-', d, swap(c), '-', swap(d)]);
                        }
                        _ => out.extend([c, swap(c)]),
                    }
                }
                _ => out.push(c),
            }
            continue;
        }
        match c {
            '[' => {
                in_class = true;
                out.push('[');
                if chars.peek() == Some(&'^') {
                    out.push(chars.next().unwrap());
                }
                if chars.peek() == Some(&']') {
                    chars.next();
                    out.push_str("\\]");
                }
            }
            '$' if !flags.multiline => out.push_str("(?=\\n?\\z)"),
            '\\' => {
                let e = chars.next().expect("trailing backslash");
                match e {
                    'Z' => out.push_str("\\z"),
                    'w' => out.push_str(&w),
                    'W' => out.push_str(&format!("[^{word}]")),
                    'd' => out.push_str(&format!("[{digit}]")),
                    'D' => out.push_str(&format!("[^{digit}]")),
                    's' => out.push_str(&format!("[{space}]")),
                    'S' => out.push_str(&format!("[^{space}]")),
                    'b' => out.push_str(&format!("(?:(?<={w})(?!{w})|(?<!{w})(?={w}))")),
                    'B' => out.push_str(&format!("(?:(?<={w})(?={w})|(?<!{w})(?!{w}))")),
                    _ => {
                        let start = out.len();
                        out.push('\\');
                        out.push(e);
                        match copy_escape_tail(e, &mut chars, &mut out) {
                            Some(c) if fold && c.is_ascii_alphabetic() => {
                                out.truncate(start);
                                out.extend(['[', c, swap(c), ']']);
                            }
                            _ => {}
                        }
                    }
                }
            }
            '(' if chars.peek() == Some(&'?') => {
                out.push('(');
                out.push(chars.next().unwrap());
                // Group names and inline flags are not pattern text, so they are copied unfolded.
                if chars.peek().is_some_and(char::is_ascii_alphabetic) {
                    for n in chars.by_ref() {
                        out.push(n);
                        if matches!(n, '>' | ')' | ':') {
                            break;
                        }
                    }
                }
            }
            c if fold && c.is_ascii_alphabetic() => out.extend(['[', c, swap(c), ']']),
            _ => out.push(c),
        }
    }
    out
}

/// Copies the hex digits of `\x`, `\u` and `\U` and the braces of `\p{..}`, so folding never touches
/// them. Returns the character a hex escape stands for.
fn copy_escape_tail(
    e: char,
    chars: &mut std::iter::Peekable<std::str::Chars>,
    out: &mut String,
) -> Option<char> {
    let count = match e {
        'x' => 2,
        'u' => 4,
        'U' => 8,
        'p' | 'P' if chars.peek() == Some(&'{') => {
            for n in chars.by_ref() {
                out.push(n);
                if n == '}' {
                    break;
                }
            }
            return None;
        }
        'p' | 'P' => 1,
        _ => return None,
    };
    let hex: String = chars.by_ref().take(count).collect();
    out.push_str(&hex);
    if e == 'p' || e == 'P' {
        return None;
    }
    u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::consts::{JOB_REL, SLUG};

    #[test]
    fn slug_is_unicode_and_full() {
        assert!(SLUG.is_full("é"));
        assert!(!SLUG.is_full("a\n"));
        assert!(SLUG.is_full("a-b_c"));
    }

    #[test]
    fn dollar_allows_one_trailing_newline() {
        static END: Pattern = Pattern::new("^a$", Flags::NONE);
        assert!(END.search("a\n").is_some());
        assert!(END.search("a\n\n").is_none());
        static MULTI: Pattern = Pattern::new("^a$", Flags::M);
        assert!(MULTI.search("b\na\n\n").is_some());
    }

    #[test]
    fn job_rel_needs_a_clean_left_edge() {
        let m = JOB_REL.search("see spec.md").unwrap();
        assert_eq!(m.get(0).unwrap().as_str(), "spec.md");
        assert!(JOB_REL.search("x/spec.md").is_none());
    }

    #[test]
    fn ascii_word_rejects_accents() {
        static WORD: Pattern = Pattern::new(r"\w+", Flags::ASCII);
        assert!(!WORD.is_full("é"));
        assert!(WORD.is_full("a_1"));
    }

    #[test]
    fn word_boundary_and_class_escapes() {
        static B: Pattern = Pattern::new(r"\bwww\.", Flags::NONE);
        assert!(B.search("a www.x").is_some());
        assert!(B.search("awww.x").is_none());
        static CLASS: Pattern = Pattern::new(r"[\w.-]+\Z", Flags::ASCII);
        assert!(CLASS.is_full("a.b-c"));
        assert!(!CLASS.is_full("a.b\n"));
        static LIT: Pattern = Pattern::new(r"[][]+", Flags::NONE);
        assert!(LIT.is_full("[]]"));
        assert_eq!(
            translate(r"\s$", Flags::ASCII),
            "[\\t\\n\\x0B\\x0C\\r ](?=\\n?\\z)"
        );
        assert_eq!(
            translate(
                r"a",
                Flags {
                    ignore_case: true,
                    multiline: true,
                    ..Flags::NONE
                }
            ),
            "(?i)(?m)a"
        );
    }

    #[test]
    fn ascii_ignore_case_folds_ascii_only() {
        static ROLE: Pattern = Pattern::new(
            r"\b(?:system prompt)\b",
            Flags {
                ascii: true,
                ignore_case: true,
                ..Flags::NONE
            },
        );
        assert!(ROLE.search("SYSTEM Prompt").is_some());
        assert!(ROLE.search("\u{17f}ystem prompt").is_none());
        static CLASS: Pattern = Pattern::new(
            r"\b[a-z0-9-]+\.[a-z]{2,}/[^.\n]\x41(?P<Name>k)",
            Flags {
                ascii: true,
                ignore_case: true,
                ..Flags::NONE
            },
        );
        assert!(CLASS.search("Ab-1.CoM/xak").is_some());
        assert!(CLASS.search("ab.c\u{212a}/xak").is_none());
        assert!(CLASS.search("ab.co/xA\u{212a}").is_none());
        static UNICODE: Pattern = Pattern::new("s", Flags::I);
        assert!(UNICODE.search("\u{17f}").is_some());
    }

    #[test]
    fn match_start_and_replace() {
        static P: Pattern = Pattern::new(r"\s*path\s*:", Flags::NONE);
        assert!(P.match_start("  path: x").is_some());
        assert!(P.match_start("x path:").is_none());
        static W: Pattern = Pattern::new(r"^W/", Flags::NONE);
        assert_eq!(W.replace_all("W/\"x\"", |_| String::new()), "\"x\"");
        let found: Vec<&str> = SLUG.find_iter("a b-c").map(|m| m.as_str()).collect();
        assert_eq!(found, ["a", "b-c"]);
    }
}
