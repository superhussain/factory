//! `pr-screenshots`: upload PR screenshots to the configured R2 bucket and print their public URLs.
//!
//! Reads CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID, PR_SCREENSHOTS_BUCKET and
//! PR_SCREENSHOTS_ORIGIN (https://...) from <home>/.env. Keys are <project>/<label>/<12 hex>-<name>;
//! without --project the project is "other". Prints one "<file>\t<url>" line per file.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crate::cli::Call;
use crate::commands::catch_up::random_hex;
use crate::shared::conf::read_env_ordered;
use crate::shared::exit::{Exit, Res};
use crate::shared::home::factory_home;
use crate::shared::http::{platform_agent, send};
use crate::shared::pyre::{Flags, Pattern};

const USAGE: &str =
    "usage: factory pr-screenshots --home <factory home> [--project <slug>] <label> <file>...";
const API: &str = "https://api.cloudflare.com/client/v4";
const NAMES: [&str; 4] = [
    "CLOUDFLARE_API_TOKEN",
    "CLOUDFLARE_ACCOUNT_ID",
    "PR_SCREENSHOTS_BUCKET",
    "PR_SCREENSHOTS_ORIGIN",
];
static SLUG: Pattern = Pattern::new("[a-z0-9][a-z0-9._-]*", Flags::NONE);
static BUCKET: Pattern = Pattern::new("[a-z0-9][a-z0-9-]{1,61}[a-z0-9]", Flags::NONE);

pub fn pr_screenshots(call: &Call) -> Res<()> {
    let home = factory_home()?;
    let mut out = io::stdout();
    match upload_all(&call.args, &home, API, &mut out) {
        Ok(()) => Ok(()),
        Err(message) => Err(Exit::raw(1, format!("{message}\n"))),
    }
}

fn content_type(file: &str) -> Option<&'static str> {
    let ext = Path::new(file).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

/// (account, token, bucket, origin) from env_path, or the message that says what is wrong.
fn publisher(env_path: &Path) -> Result<(String, String, String, String), String> {
    let env = read_env_ordered(env_path).unwrap_or_default();
    let get = |name: &str| {
        env.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let missing: Vec<&str> = NAMES.into_iter().filter(|n| get(n).is_empty()).collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} missing from {}",
            missing.join(" / "),
            env_path.display()
        ));
    }
    let [token, account, bucket, origin] = NAMES.map(get);
    if !origin.starts_with("https://") {
        return Err("PR_SCREENSHOTS_ORIGIN must start with https://".into());
    }
    if !BUCKET.is_full(&bucket) {
        return Err("PR_SCREENSHOTS_BUCKET is not a valid R2 bucket name".into());
    }
    Ok((account, token, bucket, origin.trim_end_matches('/').into()))
}

fn object_key(project: &str, label: &str, file: &str) -> String {
    let name: String = Path::new(file)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{project}/{label}/{}-{name}", random_hex(6))
}

/// Check every file, then upload each in turn. Err is the one line to print on stderr.
fn upload_all(args: &[String], home: &Path, api: &str, out: &mut dyn Write) -> Result<(), String> {
    let mut project = "other".to_string();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--project" && i + 1 < args.len() {
            project = args[i + 1].clone();
            i += 2;
        } else if args[i].starts_with("--") {
            return Err(USAGE.into());
        } else {
            positional.push(args[i].as_str());
            i += 1;
        }
    }
    let [label, files @ ..] = positional.as_slice() else {
        return Err(USAGE.into());
    };
    if files.is_empty() || !SLUG.is_full(&project) || !SLUG.is_full(label) {
        return Err(USAGE.into());
    }
    let (account, token, bucket, origin) = publisher(&home.join(".env"))?;
    for file in files {
        if !Path::new(file).is_file() {
            return Err(format!("no such file: {file}"));
        }
        if content_type(file).is_none() {
            return Err(format!("not an image: {file}"));
        }
    }
    let agent = platform_agent(api);
    for file in files {
        let key = object_key(&project, label, file);
        let url = format!("{api}/accounts/{account}/r2/buckets/{bucket}/objects/{key}");
        let data = fs::read(file).map_err(|e| format!("upload failed: {file}: {e}"))?;
        let headers = [
            ("Authorization".to_string(), format!("Bearer {token}")),
            (
                "Content-Type".to_string(),
                content_type(file).unwrap_or_default().to_string(),
            ),
        ];
        match send(agent, "PUT", &url, &headers, Some(&data), 1 << 20) {
            Ok(r) if (200..300).contains(&r.status) => {}
            Ok(r) => return Err(format!("upload failed: {file}: HTTP {}", r.status)),
            Err(e) => return Err(format!("upload failed: {file}: {e}")),
        }
        let _ = writeln!(out, "{file}\t{origin}/{key}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;
    use crate::shared::http::tests::serve;

    const LABEL: &str = "factory-236-make-pr-screenshot";
    const ENV: &str = "CLOUDFLARE_API_TOKEN=synthetic-token\nCLOUDFLARE_ACCOUNT_ID=synthetic-account\nPR_SCREENSHOTS_BUCKET=synthetic-bucket\nPR_SCREENSHOTS_ORIGIN=https://images.example.test/\n";

    fn setup(name: &str) -> (std::path::PathBuf, String) {
        let home = temp_dir(name);
        fs::write(home.join(".env"), ENV).unwrap();
        let image = home.join("after-board.png");
        fs::write(&image, b"png").unwrap();
        (home.clone(), image.to_string_lossy().into_owned())
    }

    fn run(home: &Path, api: &str, args: &[&str]) -> (Result<(), String>, String) {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let mut out = Vec::new();
        let result = upload_all(&args, home, api, &mut out);
        (result, String::from_utf8(out).unwrap())
    }

    #[test]
    fn key_with_project_and_request() {
        let (home, image) = setup("shots-project");
        let (api, seen) = serve(vec![(200, "", vec![])]);
        let (result, out) = run(&home, &api, &["--project", "factory", LABEL, &image]);
        assert_eq!(result, Ok(()));
        let prefix = format!("{image}\thttps://images.example.test/factory/{LABEL}/");
        let key = out
            .strip_prefix(&prefix)
            .unwrap()
            .strip_suffix('\n')
            .unwrap();
        assert_eq!(key.len(), 12 + "-after-board.png".len(), "{key}");
        assert!(
            key.ends_with("-after-board.png") && key[..12].chars().all(|c| c.is_ascii_hexdigit())
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].method, "PUT");
        assert_eq!(
            seen[0].path,
            format!("/accounts/synthetic-account/r2/buckets/synthetic-bucket/objects/factory/{LABEL}/{key}")
        );
        assert_eq!(
            seen[0].header("authorization"),
            Some("Bearer synthetic-token")
        );
        assert_eq!(seen[0].header("content-type"), Some("image/png"));
        assert_eq!(seen[0].body, b"png");
    }

    #[test]
    fn key_without_project() {
        let (home, image) = setup("shots-other");
        let (api, _) = serve(vec![(200, "", vec![])]);
        let (result, out) = run(&home, &api, &[LABEL, &image]);
        assert_eq!(result, Ok(()));
        assert!(
            out.contains(&format!("https://images.example.test/other/{LABEL}/")),
            "{out}"
        );
    }

    #[test]
    fn bad_config_and_files_fail_before_any_upload() {
        let (home, image) = setup("shots-config");
        let api = "http://127.0.0.1:9";
        fs::write(
            home.join(".env"),
            "CLOUDFLARE_API_TOKEN=\nCLOUDFLARE_ACCOUNT_ID=synthetic-account\n",
        )
        .unwrap();
        let env = home.join(".env");
        assert_eq!(
            run(&home, api, &[LABEL, &image]).0,
            Err(format!("CLOUDFLARE_API_TOKEN / PR_SCREENSHOTS_BUCKET / PR_SCREENSHOTS_ORIGIN missing from {}", env.display()))
        );
        for (line, message) in [
            (
                "PR_SCREENSHOTS_ORIGIN=http://images.example.test",
                "PR_SCREENSHOTS_ORIGIN must start with https://",
            ),
            (
                "PR_SCREENSHOTS_BUCKET=Synthetic_Bucket",
                "PR_SCREENSHOTS_BUCKET is not a valid R2 bucket name",
            ),
        ] {
            fs::write(&env, format!("{ENV}{line}\n")).unwrap();
            assert_eq!(
                run(&home, api, &[LABEL, &image]).0,
                Err(message.to_string())
            );
        }
        fs::write(&env, ENV).unwrap();
        let gone = home.join("gone.png").to_string_lossy().into_owned();
        assert_eq!(
            run(&home, api, &[LABEL, &image, &gone]).0,
            Err(format!("no such file: {gone}"))
        );
        let text = home.join("notes.txt");
        fs::write(&text, "x").unwrap();
        let text = text.to_string_lossy().into_owned();
        assert_eq!(
            run(&home, api, &[LABEL, &image, &text]).0,
            Err(format!("not an image: {text}"))
        );
    }

    #[test]
    fn http_error_hides_the_token() {
        let (home, image) = setup("shots-403");
        let (api, _) = serve(vec![(403, "", vec![])]);
        let (result, out) = run(&home, &api, &[LABEL, &image]);
        let err = result.unwrap_err();
        assert_eq!(err, format!("upload failed: {image}: HTTP 403"));
        assert!(!(err + &out).contains("synthetic-token"));
    }

    #[test]
    fn usage() {
        let (home, image) = setup("shots-usage");
        for args in [
            vec!["Factory-236", image.as_str()],
            vec!["x\n", image.as_str()],
            vec![LABEL],
            vec!["--bogus", LABEL, image.as_str()],
            vec!["--project", "Bad", LABEL, image.as_str()],
        ] {
            assert_eq!(
                run(&home, "http://127.0.0.1:9", &args).0,
                Err(USAGE.to_string()),
                "{args:?}"
            );
        }
    }

    #[test]
    fn env_parsing() {
        let home = temp_dir("shots-env");
        let path = home.join(".env");
        fs::write(
            &path,
            b"# comment\r\n\r\nA=\"quoted value\"\r\nB='single'\r\n  C = plain \r\nnoequals\r\n",
        )
        .unwrap();
        assert_eq!(
            read_env_ordered(&path).unwrap(),
            [("A", "quoted value"), ("B", "single"), ("C", "plain")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
        );
    }
}
