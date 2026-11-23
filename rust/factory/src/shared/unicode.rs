//! A per-character NFKC fold from Python's `unicodedata`. Folding each character alone skips
//! composition across characters, so a check that rejects folded text can only over-reject.

use super::nfkc_table::NFKC;

/// Each character replaced by its own NFKC form.
pub fn nfkc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match NFKC.binary_search_by_key(&(c as u32), |&(cp, _)| cp) {
            Ok(i) => out.push_str(NFKC[i].1),
            Err(_) => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_matches_python() {
        assert!(NFKC.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(NFKC.len(), 4796);
        assert_eq!(nfkc("ｆﬁ①é\u{a0}\u{2126}\u{3000}ｶ"), "ffi1é Ω カ");
        assert_eq!(nfkc("operator-preferences"), "operator-preferences");
    }
}
