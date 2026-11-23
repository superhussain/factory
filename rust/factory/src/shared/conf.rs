//! `.env` files and markdown frontmatter.

use std::io;
use std::path::Path;

use super::fs::{read_text, regular_metadata};
use super::py::{lstrip, splitlines, strip};

/// A value without one pair of matching outer quotes.
pub fn unquote(v: &str) -> &str {
    let b = v.as_bytes();
    if b.len() >= 2 && b[0] == b[b.len() - 1] && (b[0] == b'"' || b[0] == b'\'') {
        return &v[1..v.len() - 1];
    }
    v
}

pub type Conf = std::collections::BTreeMap<String, String>;

/// `KEY=value` lines in file order, skipping blanks, comments and lines with no `=`. A repeated key
/// keeps its first place and its last value, as a dict does.
pub fn read_env_ordered(p: &Path) -> io::Result<Vec<(String, String)>> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for line in read_text(p)?.split('\n') {
        let line = strip(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let (key, value) = (strip(key).to_string(), unquote(strip(value)).to_string());
            match pairs.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => pairs.push((key, value)),
            }
        }
    }
    Ok(pairs)
}

/// `read_conf`.
pub fn read_conf(p: &Path) -> io::Result<Conf> {
    Ok(read_env_ordered(p)?.into_iter().collect())
}

/// `frontmatter`: the flat `key: value` block between `---` lines, in file order, and the body.
pub fn frontmatter(p: &Path) -> io::Result<(Vec<(String, String)>, String)> {
    let text = read_text(p)?;
    let lines = splitlines(&text, true);
    if lines.first().map(|l| strip(l)) != Some("---") {
        return Ok((Vec::new(), text));
    }
    let mut meta = Vec::new();
    for (i, line) in lines.iter().enumerate().skip(1) {
        let line = strip(line);
        if line == "---" {
            let body = lines[i + 1..].concat();
            return Ok((meta, body));
        }
        if !line.is_empty() && !line.starts_with('#') {
            if let Some((key, value)) = line.split_once(':') {
                meta.push((strip(key).to_string(), unquote(strip(value)).to_string()));
            }
        }
    }
    Ok((Vec::new(), text))
}

/// A frontmatter value; the last one wins, as a dict does.
pub fn fm_get<'a>(meta: &'a [(String, String)], key: &str) -> Option<&'a str> {
    meta.iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// `closed_frontmatter`: a strict frontmatter block that must close, with unique keys.
pub fn closed_frontmatter(p: &Path) -> Result<Vec<(String, String)>, ()> {
    regular_metadata(p)?;
    let text = read_text(p).map_err(|_| ())?;
    let lines = splitlines(&text, false);
    if lines.first() != Some(&"---") {
        return Err(());
    }
    let mut values: Vec<(String, String)> = Vec::new();
    for line in &lines[1..] {
        if *line == "---" {
            return Ok(values);
        }
        if strip(line).is_empty() || lstrip(line).starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(':').ok_or(())?;
        if strip(key).is_empty() || key != strip(key) || values.iter().any(|(k, _)| k == key) {
            return Err(());
        }
        values.push((key.to_string(), unquote(strip(value)).to_string()));
    }
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;
    use std::fs;

    #[test]
    fn frontmatter_reads_crlf() {
        let dir = temp_dir("conf");
        let p = dir.join("config.md");
        fs::write(
            &p,
            "---\r\napi_url: 'http://x'\r\n# note\r\ndevice: a\r\ndevice: b\r\n---\r\nbody\r\n",
        )
        .unwrap();
        let (meta, body) = frontmatter(&p).unwrap();
        assert_eq!(fm_get(&meta, "api_url"), Some("http://x"));
        assert_eq!(fm_get(&meta, "device"), Some("b"));
        assert_eq!(body, "body\n");
        assert!(closed_frontmatter(&p).is_err());
        fs::write(&p, "---\nsample: /srv/sample\n\n---\n").unwrap();
        assert_eq!(
            closed_frontmatter(&p).unwrap(),
            vec![("sample".to_string(), "/srv/sample".to_string())]
        );
        fs::write(&p, "---\nopen: yes\n").unwrap();
        assert_eq!(
            frontmatter(&p).unwrap(),
            (Vec::new(), "---\nopen: yes\n".to_string())
        );
        assert!(closed_frontmatter(&p).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_conf_parses_env_lines() {
        let dir = temp_dir("env");
        let p = dir.join(".env");
        fs::write(&p, "# c\nA = \"1\"\r\nnoequals\nB=x=y\n").unwrap();
        let conf = read_conf(&p).unwrap();
        assert_eq!(conf.get("A").map(String::as_str), Some("1"));
        assert_eq!(conf.get("B").map(String::as_str), Some("x=y"));
        assert_eq!(conf.len(), 2);
        fs::write(&p, "Z=1\nA=2\nZ=3\n").unwrap();
        assert_eq!(
            read_env_ordered(&p).unwrap(),
            vec![
                ("Z".to_string(), "3".to_string()),
                ("A".to_string(), "2".to_string())
            ]
        );
        assert_eq!(unquote("'"), "'");
        fs::remove_dir_all(dir).unwrap();
    }
}
