#!/usr/bin/env python3
"""Upload PR screenshots to the configured R2 bucket and print their public URLs.

Usage: pr_screenshots.py --home <factory home> [--project <slug>] <label> <file>...
Reads CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID, PR_SCREENSHOTS_BUCKET and
PR_SCREENSHOTS_ORIGIN (https://...) from <home>/.env.
Keys are <project>/<label>/<12 hex>-<name>; without --project the project is "other".
Prints one "<file>\t<url>" line per file.
"""
import argparse
import re
import secrets
import sys
import urllib.error
import urllib.request
from pathlib import Path

USAGE = "usage: pr_screenshots.py --home <factory home> [--project <slug>] <label> <file>..."
SLUG = re.compile(r"[a-z0-9][a-z0-9._-]*")
BUCKET = re.compile(r"[a-z0-9][a-z0-9-]{1,61}[a-z0-9]")
TYPES = {".png": "image/png", ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".gif": "image/gif", ".webp": "image/webp"}


def read_env(path):
    env = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = (s.strip() for s in line.split("=", 1))
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                value = value[1:-1]
            env[key] = value
    return env


def publisher(env_path):
    """(account, token, bucket, origin) from env_path, or an error message."""
    try:
        env = read_env(env_path)
    except OSError:
        env = {}
    names = ("CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID", "PR_SCREENSHOTS_BUCKET", "PR_SCREENSHOTS_ORIGIN")
    missing = [name for name in names if not env.get(name)]
    if missing:
        return f"{' / '.join(missing)} missing from {env_path}"
    token, account, bucket, origin = (env[name] for name in names)
    if not origin.startswith("https://"):
        return "PR_SCREENSHOTS_ORIGIN must start with https://"
    if not BUCKET.fullmatch(bucket):
        return "PR_SCREENSHOTS_BUCKET is not a valid R2 bucket name"
    return account, token, bucket, origin.rstrip("/")


def object_key(project, label, path):
    name = re.sub(r"[^A-Za-z0-9._-]", "-", Path(path).name)
    return f"{project}/{label}/{secrets.token_hex(6)}-{name}"


def upload(account, token, bucket, key, path, content_type):
    url = f"https://api.cloudflare.com/client/v4/accounts/{account}/r2/buckets/{bucket}/objects/{key}"
    request = urllib.request.Request(
        url,
        data=Path(path).read_bytes(),
        method="PUT",
        headers={"Authorization": f"Bearer {token}", "Content-Type": content_type},
    )
    urllib.request.urlopen(request, timeout=60).close()


def fail(message):
    print(message, file=sys.stderr)
    return 1


def main(argv=None):
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--home", required=True)
    parser.add_argument("--project", default="other")
    parser.add_argument("label")
    parser.add_argument("files", nargs="+")
    parser.error = lambda message: (_ for _ in ()).throw(SystemExit(fail(USAGE)))
    args = parser.parse_args(argv)
    if not (SLUG.fullmatch(args.project) and SLUG.fullmatch(args.label) and Path(args.home).is_absolute()):
        return fail(USAGE)

    config = publisher(Path(args.home) / ".env")
    if isinstance(config, str):
        return fail(config)
    account, token, bucket, origin = config

    for f in args.files:
        if not Path(f).is_file():
            return fail(f"no such file: {f}")
        if Path(f).suffix.lower() not in TYPES:
            return fail(f"not an image: {f}")

    for f in args.files:
        key = object_key(args.project, args.label, f)
        try:
            upload(account, token, bucket, key, f, TYPES[Path(f).suffix.lower()])
        except urllib.error.HTTPError as e:
            return fail(f"upload failed: {f}: HTTP {e.code}")
        except (urllib.error.URLError, OSError) as e:
            return fail(f"upload failed: {f}: {getattr(e, 'reason', e)}")
        print(f"{f}\t{origin}/{key}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
