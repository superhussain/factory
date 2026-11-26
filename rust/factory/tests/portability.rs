//! Ported from tests/cli/test_portability.py: what the installed skill folder ships must carry no
//! absolute host paths, resolve its relative links and keep SKILL.md small and indexing every command.

use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

const SKILL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../skills/factory");

/// Every shipped file under the skill folder, skipping dot-prefixed and __pycache__ parts.
fn shipped(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "__pycache__" {
            continue;
        }
        if path.is_dir() {
            shipped(&path, out);
        } else if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("pyc" | "pyo")
        ) {
            out.push(path);
        }
    }
}

#[test]
fn exact_bundle_and_relative_installed_references() {
    let root = Path::new(SKILL);
    let mut files = Vec::new();
    shipped(root, &mut files);
    assert!(!files.is_empty());
    let absolute = Regex::new(r"(?:^|\s)(?:/home/|/[Uu]sers/|[A-Za-z]:\\Users\\)\S+").unwrap();
    let link = Regex::new(r"\]\(([^)]+)\)").unwrap();
    for path in files {
        let name = path.strip_prefix(root).unwrap().display().to_string();
        let text = String::from_utf8(fs::read(&path).unwrap())
            .unwrap_or_else(|_| panic!("{name} is not UTF-8"));
        assert!(!text.contains("file://"), "{name}");
        assert!(!absolute.is_match(&text), "{name}");
        if name.ends_with(".md") {
            for cap in link.captures_iter(&text) {
                let target = &cap[1];
                if ["references/", "execution.md", "integrations.md"]
                    .iter()
                    .any(|p| target.starts_with(p))
                {
                    let file = path
                        .parent()
                        .unwrap()
                        .join(target.split('#').next().unwrap());
                    assert!(file.is_file(), "{name}: {target}");
                }
            }
        }
    }
}

#[test]
fn skill_md_is_small_and_indexes_every_command() {
    let root = Path::new(SKILL);
    let skill = fs::read_to_string(root.join("SKILL.md")).unwrap();
    assert!(skill.len() < 37500, "SKILL.md is {} bytes", skill.len());
    let mut commands: Vec<_> = fs::read_dir(root.join("references/commands"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    commands.sort();
    assert!(!commands.is_empty());
    for name in commands {
        assert!(
            skill.contains(&format!("](references/commands/{name})")),
            "{name}"
        );
    }
}
