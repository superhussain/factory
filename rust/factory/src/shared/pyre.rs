//! Python `re` patterns on fancy-regex: `translate` rewrites the parts whose meaning differs.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use fancy_regex::{Regex, RegexBuilder};

use super::py::repr;

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
    limit: usize,
    search: OnceLock<Regex>,
    start: OnceLock<Regex>,
    full: OnceLock<Regex>,
}

const BACKTRACK_LIMIT: usize = 100_000_000;

/// The words every backtrack-limit stop carries, so a caller can tell that stop from other failures.
pub const LIMIT_HIT: &str = "hit the regex backtrack limit";

static WARNED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
thread_local! {
    /// A backtrack limit every pattern uses in this test thread instead of its own, when set, so a
    /// test can reach the limit on short text.
    pub(crate) static TEST_LIMIT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// The one pattern TEST_LIMIT applies to, when set, so a test can stop one gate of several.
    pub(crate) static TEST_LIMIT_ONLY: std::cell::Cell<Option<*const Pattern>> = const { std::cell::Cell::new(None) };
}

impl Pattern {
    pub const fn new(src: &'static str, flags: Flags) -> Pattern {
        Pattern::with_limit(src, flags, BACKTRACK_LIMIT)
    }

    const fn with_limit(src: &'static str, flags: Flags, limit: usize) -> Pattern {
        Pattern {
            src,
            flags,
            limit,
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
        let build = |limit| {
            let source = format!(
                "{}{before}{}{after}",
                prefix(self.flags),
                body(self.src, self.flags)
            );
            RegexBuilder::new(&source)
                .backtrack_limit(limit)
                .build()
                .unwrap_or_else(|e| panic!("bad pattern {:?}: {e}", self.src))
        };
        #[cfg(test)]
        if let Some(limit) = TEST_LIMIT.with(std::cell::Cell::get) {
            if TEST_LIMIT_ONLY
                .with(std::cell::Cell::get)
                .is_none_or(|p| std::ptr::eq(p, self))
            {
                return Box::leak(Box::new(build(limit)));
            }
        }
        lock.get_or_init(|| build(self.limit))
    }

    /// A search that hit the backtrack limit counts as no match, with one warning per process.
    fn no_match<T>(&self, result: Result<Option<T>, fancy_regex::Error>) -> Option<T> {
        result.unwrap_or_else(|_| {
            if !WARNED.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "pattern {} hit the regex backtrack limit; treating it as no match",
                    repr(self.src)
                );
            }
            None
        })
    }

    /// `re.search`.
    pub fn search<'t>(&self, s: &'t str) -> Option<Captures<'t>> {
        self.no_match(self.compiled("search").captures(s))
    }

    /// `re.match`.
    pub fn match_start<'t>(&self, s: &'t str) -> Option<Captures<'t>> {
        self.no_match(self.compiled("start").captures(s))
    }

    /// `re.fullmatch`.
    pub fn fullmatch<'t>(&self, s: &'t str) -> Option<Captures<'t>> {
        self.no_match(self.compiled("full").captures(s))
    }

    pub fn is_full(&self, s: &str) -> bool {
        self.no_match(self.compiled("full").is_match(s).map(Some))
            .unwrap_or(false)
    }

    /// `re.finditer`, as matches. It stops at a search that hit the backtrack limit.
    pub fn find_iter<'r, 't>(
        &'r self,
        s: &'t str,
    ) -> impl Iterator<Item = fancy_regex::Match<'t>> + use<'r, 't> {
        self.compiled("search")
            .find_iter(s)
            .map_while(|m| self.no_match(m.map(Some)))
    }

    /// `re.sub` with a function. Past the backtrack limit it exits, so unredacted text is never written.
    pub fn replace_all(&self, s: &str, f: impl FnMut(&Captures) -> String) -> String {
        self.replace_cow(s, f).into_owned()
    }

    /// `replace_all`, borrowing s when nothing matched.
    pub fn replace_cow<'t>(&self, s: &'t str, f: impl FnMut(&Captures) -> String) -> Cow<'t, str> {
        self.compiled("search")
            .try_replacen(s, 0, f)
            .unwrap_or_else(|_| self.stop())
    }

    /// A linear-time superset of this pattern: its translation with every lookaround dropped, so a
    /// text this pattern can match always matches it too.
    pub fn linear(&self) -> String {
        drop_lookaround(&translate(self.src, self.flags))
    }

    /// `re.search` for a redaction gate. Past the backtrack limit it exits rather than count it as no match.
    pub fn search_or_stop<'t>(&self, s: &'t str) -> Option<Captures<'t>> {
        self.compiled("search")
            .captures(s)
            .unwrap_or_else(|_| self.stop())
    }

    /// `re.fullmatch` for a redaction gate. Past the backtrack limit it exits.
    pub fn is_full_or_stop(&self, s: &str) -> bool {
        self.compiled("full")
            .is_match(s)
            .unwrap_or_else(|_| self.stop())
    }

    /// `re.finditer` for a redaction gate. Past the backtrack limit it exits.
    pub fn find_all_or_stop<'t>(&self, s: &'t str) -> Vec<fancy_regex::Match<'t>> {
        self.compiled("search")
            .find_iter(s)
            .map(|m| m.unwrap_or_else(|_| self.stop()))
            .collect()
    }

    /// `re.search` for a gate whose caller decides what a limit hit stops: Err holds the reason.
    pub fn search_or_err<'t>(&self, s: &'t str) -> Result<Option<Captures<'t>>, String> {
        self.compiled("search")
            .captures(s)
            .map_err(|_| self.limit_reason())
    }

    /// Why a search past the backtrack limit stopped, naming the pattern.
    fn limit_reason(&self) -> String {
        format!(
            "pattern {} {LIMIT_HIT}; stopping rather than pass text it could not finish reading; shorten or split the text",
            repr(self.src)
        )
    }

    /// Exit 1 so a redaction or refusal never passes text a pattern could not finish reading.
    fn stop(&self) -> ! {
        let msg = format!("{}", self.limit_reason());
        #[cfg(test)]
        panic!("{msg}");
        #[cfg(not(test))]
        {
            eprintln!("{msg}");
            std::process::exit(1)
        }
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

/// A translated pattern without its lookaround groups, `\b` and `$` included, since translate writes
/// them as lookarounds.
fn drop_lookaround(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars();
    let (mut depth, mut skip_from, mut in_class) = (0, None, false);
    while let Some(c) = chars.next() {
        let mut piece = String::from(c);
        match c {
            '\\' => piece.extend(chars.next()),
            '[' => in_class = true,
            ']' => in_class = false,
            '(' if !in_class => {
                depth += 1;
                let rest = chars.as_str();
                if skip_from.is_none()
                    && ["?=", "?!", "?<=", "?<!"]
                        .iter()
                        .any(|p| rest.starts_with(p))
                {
                    skip_from = Some(depth);
                }
            }
            ')' if !in_class => {
                depth -= 1;
                if skip_from == Some(depth + 1) {
                    skip_from = None;
                    continue;
                }
            }
            _ => {}
        }
        if skip_from.is_none() {
            out.push_str(&piece);
        }
    }
    out
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
    use crate::shared::consts::{JOB_REL, PROJECT_SLUG, SLUG};

    #[test]
    fn slug_is_unicode_and_full() {
        assert!(SLUG.is_full("é"));
        assert!(!SLUG.is_full("a\n"));
        assert!(SLUG.is_full("a-b_c"));
    }

    #[test]
    fn project_slug_is_ascii() {
        assert!(PROJECT_SLUG.is_full("a-b_C9"));
        assert!(!PROJECT_SLUG.is_full("café"));
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

    #[test]
    #[should_panic(expected = "hit the regex backtrack limit; stopping")]
    fn redaction_gate_stops_at_the_backtrack_limit() {
        let p = Pattern::with_limit(
            r"\bgit\b[^;&|\n]*\bpush\b[^;&|\n]*\s(?:--force(?:-with-lease)?|-f)(?=[\s=]|$)",
            Flags::NONE,
            1_000,
        );
        p.search_or_stop(&"git ".repeat(500));
    }

    #[test]
    fn backtrack_limit_is_no_match() {
        let p = Pattern::with_limit(
            r"\bgit\b[^;&|\n]*\bpush\b[^;&|\n]*\s(?:--force(?:-with-lease)?|-f)(?=[\s=]|$)",
            Flags::NONE,
            1_000,
        );
        assert!(p.search(&"git ".repeat(500)).is_none());
    }

    #[test]
    fn drop_lookaround_keeps_only_linear_parts() {
        for (src, flags, linear) in [
            (r"a(?=b(?!c))d", Flags::NONE, "ad"),
            (r"(?<=x)a(?<!y)", Flags::NONE, "a"),
            (r"\bx\B", Flags::ASCII, "(?:|)x(?:|)"),
            (r"a$", Flags::NONE, "a"),
            (r"a$", Flags::M, "(?m)a$"),
            (r"[(?=]x\(?!", Flags::NONE, r"[(?=]x\(?!"),
        ] {
            let out = drop_lookaround(&translate(src, flags));
            assert_eq!(out, linear, "{src}");
            regex::Regex::new(&out).unwrap();
        }
    }
}
