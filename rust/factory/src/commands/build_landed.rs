//! The build-landed command: hold the job, read the latest build.md run line, push the branch
//! and the job folder.

/// ("Run N", the first non-empty line after the last `## Run N` heading).
pub(crate) fn latest_run_line(text: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let (at, run) = lines.iter().enumerate().rev().find_map(|(i, line)| {
        let n = line.strip_prefix("## Run ")?.trim_end();
        (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then(|| (i, format!("Run {n}")))
    })?;
    let first = lines[at + 1..]
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())?;
    Some((run, first.to_string()))
}

#[cfg(test)]
mod tests {
    use super::latest_run_line;

    #[test]
    fn build_landed_picks_the_last_run() {
        let text = "# Build\n\n## Run 1\n\nRouting: one.\n\n## Run 2\n\nRouting: two.\nmore\n";
        assert_eq!(
            latest_run_line(text),
            Some(("Run 2".to_string(), "Routing: two.".to_string()))
        );
    }

    #[test]
    fn build_landed_needs_a_run_heading() {
        assert_eq!(latest_run_line("# Build\n\nRouting: one.\n"), None);
    }

    #[test]
    fn build_landed_needs_a_line_after_the_heading() {
        assert_eq!(
            latest_run_line("## Run 1\n\nfirst\n\n## Run 2\n\n  \n"),
            None
        );
    }
}
