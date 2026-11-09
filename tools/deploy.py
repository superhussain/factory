#!/usr/bin/env python3
"""Private deployment authority: one JSON profile projects the fixed Wrangler config.

Commands: check | render | dry-run | deploy, each with --profile <file>.
Only ignored deploy/profile.json is deployment input. Exit 2 for invalid
profile/options, 1 for failed build/Wrangler checks, 0 on success.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys

COMMANDS = ("check", "render", "dry-run", "deploy")
COMPATIBILITY_DATE = "2026-06-01"
PROFILE_KEYS = ["version", "account_id", "worker_name", "bucket_name", "origin", "title",
                "issuer", "audience", "image_origins", "agent_links"]
OPTIONAL_PROFILE_KEYS = ["jira_origin"]

DNS = re.compile(r"^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z][a-z0-9-]*$")
CONTROLS = re.compile(r"[\x00-\x1f\x7f-\x9f]")
ACCOUNT_ID = re.compile(r"^[0-9a-f]{32}$")
WORKER_NAME = re.compile(r"^[a-z0-9-]{1,63}$")
BUCKET_NAME = re.compile(r"^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$")
IPV4_SHAPED = re.compile(r"^\d+\.\d+\.\d+\.\d+$")
LABELS = re.compile(r"^[a-zA-Z0-9_-]{1,64}$")


class ProfileError(ValueError):
    pass


def fail(message):
    raise ProfileError(message)


def text(value, name, maximum=None):
    if not isinstance(value, str):
        fail(name + " must be a string")
    if not value.strip():
        fail(name + " must be nonempty")
    if CONTROLS.search(value):
        fail(name + " must not contain control characters")
    if "REPLACE_ME" in value or "${" in value or re.search(r"<[a-z][a-z0-9-]*>", value):
        fail(name + " contains an unresolved placeholder")
    if maximum is not None and len(value) > maximum:
        fail(name + " is over " + str(maximum) + " code points")
    return value


def https_origin(value, name, allow_ip=False):
    text(value, name)
    match = re.match(r"^https://([^/?#]+)$", value)
    if not match:
        fail(name + " must be a canonical https origin without path, query or fragment")
    host = match.group(1)
    if "@" in host:
        fail(name + " must not contain credentials")
    if ":" in host:
        if not allow_ip:
            fail(name + " must not contain an explicit port")
        host, _, port = host.rpartition(":")
        if port != "443":
            fail(name + " must not contain an explicit nondefault port")
    if host.startswith("xn--") or "*" in host:
        fail(name + " must not be a wildcard or punycode-ata origin")
    if not (DNS.match(host) or (allow_ip and re.match(r"^(\d+\.\d+\.\d+\.\d+|\[[0-9a-f:]+\])$", host))):
        fail(name + " needs a valid DNS hostname")
    return host


def image_origins(value):
    if not isinstance(value, list) or any(not isinstance(v, str) for v in value):
        fail("image_origins must be a list of origin strings")
    if len(set(value)) != len(value):
        fail("image_origins must not contain duplicates")
    for origin in value:
        https_origin(origin, "image_origins entry")
    return list(value)


def link_url(template, kind):
    substituted = template.replace("{server}", "server-id").replace("{agent}", "agent-id")
    if substituted.startswith("//"):
        fail(kind + " link template must not be protocol-relative")
    match = re.match(r"^([a-z][a-z0-9+.-]*):(.*)$", substituted, re.S)
    if not match:
        fail(kind + " link template must be an absolute URL")
    scheme = match.group(1)
    if scheme in ("javascript", "data", "file", "blob", "http"):
        fail(kind + " link template scheme " + scheme + " is not allowed")
    if kind == "web" and scheme != "https":
        fail("web link template must be https")
    if kind == "app" and scheme in ("http", "https"):
        fail("app link template needs a non-http custom scheme")
    if re.search(r"//[^/]*@", substituted):
        fail(kind + " link template must not contain credentials")
    if CONTROLS.search(substituted):
        fail(kind + " link template must not contain control characters")


def agent_links(value):
    if not isinstance(value, dict):
        fail("agent_links must be an object")
    for kind, link in value.items():
        if kind not in ("app", "web"):
            fail("agent_links allows only app and web keys")
        if not isinstance(link, dict) or sorted(link.keys()) != ["label", "template"]:
            fail(kind + " link needs exactly label and template")
        text(link["label"], kind + " link label", maximum=160)
        template = text(link["template"], kind + " link template")
        placeholders = re.findall(r"\{[^{}]*\}", template)
        if len(placeholders) != 2 or "{server}" not in placeholders or "{agent}" not in placeholders:
            fail(kind + " link template needs exactly the {server} and {agent} placeholders")
        link_url(template, kind)
    return dict(value)


def load_object(path):
    try:
        raw = Path(path).read_text(encoding="utf-8")
    except OSError as error:
        fail("cannot read profile: " + error.strerror or str(error))

    def pairs(items):
        seen = {}
        for key, value in items:
            if key in seen:
                fail("duplicate key " + json.dumps(key))
            seen[key] = value
        return seen

    try:
        data = json.loads(raw, object_pairs_hook=pairs)
    except json.JSONDecodeError as error:
        fail("profile is not valid JSON: " + str(error))
    if not isinstance(data, dict):
        fail("profile must be a JSON object")
    return data


def load_profile(path):
    """Read and fully validate the private profile into a frozen boundary value."""
    data = load_object(path)
    unknown = sorted(set(data) - set(PROFILE_KEYS) - set(OPTIONAL_PROFILE_KEYS))
    if unknown:
        fail("unknown profile keys: " + ", ".join(unknown))
    missing = [key for key in PROFILE_KEYS if key not in data]
    if missing:
        fail("missing profile keys: " + ", ".join(missing))
    if data["version"] != 1 or isinstance(data["version"], bool):
        fail("version must be the number 1")
    account_id = text(data["account_id"], "account_id")
    if not ACCOUNT_ID.match(account_id):
        fail("account_id must be 32 hexadecimal characters")
    worker_name = text(data["worker_name"], "worker_name")
    if not WORKER_NAME.match(worker_name):
        fail("worker_name must be 1-63 lowercase letters, digits or hyphens")
    bucket_name = text(data["bucket_name"], "bucket_name")
    if (not BUCKET_NAME.match(bucket_name) or IPV4_SHAPED.match(bucket_name)
            or bucket_name.startswith(("xn--", "sthree-", "sthree-configurator"))
            or bucket_name.endswith(("-s3alias", "--ol-s3"))):
        fail("bucket_name is not an allowed bucket name")
    origin_host = https_origin(data["origin"], "origin")
    if origin_host == "workers.dev" or origin_host.endswith(".workers.dev"):
        fail("origin must be a custom domain, not workers.dev")
    issuer_host = https_origin(data["issuer"], "issuer")
    if not re.match(r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.cloudflareaccess\.com$", issuer_host):
        fail("issuer must be a canonical https <team>.cloudflareaccess.com origin")
    audience = text(data["audience"], "audience")
    title = text(data["title"], "title", maximum=160)
    profile = {
        "version": 1,
        "account_id": account_id,
        "worker_name": worker_name,
        "bucket_name": bucket_name,
        "origin": data["origin"],
        "title": title,
        "issuer": data["issuer"],
        "audience": audience,
        "image_origins": image_origins(data["image_origins"]),
        "agent_links": agent_links(data["agent_links"]),
    }
    if "jira_origin" in data:
        https_origin(data["jira_origin"], "jira_origin")
        profile["jira_origin"] = data["jira_origin"]
    return profile


def write_atomic(path, document):
    path = Path(path)
    temporary = path.with_name(path.name + "." + str(os.getpid()) + ".tmp")
    temporary.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def project_wrangler(profile, source_root, output):
    """Atomically render the fixed production Wrangler config and connection descriptor."""
    source_root = Path(source_root).resolve()
    main = source_root / "worker" / "index.js"
    if not main.is_file():
        fail("worker/index.js not found under " + str(source_root))
    config = {
        "name": profile["worker_name"],
        "account_id": profile["account_id"],
        "main": str(main),
        "compatibility_date": COMPATIBILITY_DATE,
        "workers_dev": False,
        "preview_urls": False,
        "routes": [{"pattern": profile["origin"].split("://", 1)[1], "custom_domain": True}],
        "r2_buckets": [{"binding": "BUCKET", "bucket_name": profile["bucket_name"]}],
        "ai": {"binding": "AI"},
        "durable_objects": {"bindings": [{"name": "BoardEvents", "class_name": "BoardEvents"}]},
        "migrations": [{"tag": "v1", "new_sqlite_classes": ["BoardEvents"]}],
        "vars": {
            "FACTORY_ORIGIN": profile["origin"],
            "FACTORY_TITLE": profile["title"],
            "ACCESS_ISSUER": profile["issuer"],
            "ACCESS_AUDIENCE": profile["audience"],
            "FACTORY_IMAGE_ORIGINS": json.dumps(profile["image_origins"]),
            "FACTORY_AGENT_LINKS": json.dumps(profile["agent_links"], sort_keys=True),
        },
    }
    if "jira_origin" in profile:
        config["vars"]["FACTORY_JIRA_ORIGIN"] = profile["jira_origin"]
    connection = {"version": 1, "api_url": profile["origin"]}
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    config_path = output / "wrangler.production.json"
    write_atomic(config_path, config)
    write_atomic(output / "connection.json", connection)
    return config_path


def wrangler(source_root, args):
    executable = Path(source_root) / "node_modules" / ".bin" / "wrangler"
    if not executable.is_file():
        fail("locked Wrangler is not installed; run pnpm install --frozen-lockfile first")
    result = subprocess.run([str(executable)] + args, capture_output=True, text=True)
    return result


def node(source_root, expression):
    return subprocess.run(["node", "--input-type=module", "-e", expression],
                          cwd=source_root, capture_output=True, text=True)


def node_tooling_problem(source_root):
    if not (Path(source_root) / "node_modules" / ".bin" / "wrangler").is_file():
        return "locked Wrangler is not installed; run mise exec -- pnpm install --frozen-lockfile first"
    engines = json.loads((Path(source_root) / "package.json").read_text())["engines"]["node"]
    minimum = tuple(int(part) for part in engines.lstrip(">=").split("."))
    try:
        result = subprocess.run(["node", "--version"], capture_output=True, text=True)
    except FileNotFoundError:
        return "node is not on PATH; run this command through mise exec --"
    found = result.stdout.strip()
    error = " ".join(result.stderr.split())
    if not found and error:
        return "node --version failed: " + error + "; run this command through mise exec --"
    match = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?", found)
    if match is None:
        return 'could not read node --version output "' + found + '"; run this command through mise exec --'
    if tuple(int(part) for part in match.groups()) < minimum:
        return ("node " + found + " is older than package.json engines " + engines +
                "; run this command through mise exec --")
    return None


def main(argv=None):
    args = list(sys.argv[1:] if argv is None else argv)
    if len(args) != 3 or args[0] not in COMMANDS or args[1] != "--profile":
        print("usage: python3 tools/deploy.py <check|render|dry-run|deploy> --profile <file>", file=sys.stderr)
        return 2
    command, profile_path = args[0], args[2]
    source_root = Path(__file__).resolve().parent.parent
    build = source_root / ".build"
    try:
        profile = load_profile(profile_path)
        if command == "check":
            print("profile ok: " + profile["worker_name"])
            return 0
        config_path = project_wrangler(profile, source_root, build)
        if command == "render":
            print(config_path)
            print(build / "connection.json")
            return 0
        problem = node_tooling_problem(source_root)
        if problem:
            sys.stderr.write("preflight: " + problem + "\n")
            return 1
        assets = node(source_root, "import('./tools/assets.mjs').then(m => m.prepareAssets())")
        if assets.returncode != 0:
            sys.stderr.write(assets.stderr)
            return 1
        outdir = build / "worker"
        result = wrangler(source_root, ["deploy", "--dry-run", "--outdir", str(outdir),
                                        "--outfile", str(outdir / "upload.form"),
                                        "--config", str(config_path)])
        if result.returncode != 0:
            sys.stderr.write(result.stderr)
            return 1
        expression = ("import('./tools/assets.mjs').then(m => m.inspectBundle(" +
                      json.dumps(str(outdir)) + ", " + json.dumps(result.stdout + result.stderr) +
                      ")).then(r => console.log(JSON.stringify(r)))")
        inspect = node(source_root, expression)
        if inspect.returncode != 0:
            sys.stderr.write(inspect.stderr)
            return 1
        print(inspect.stdout.strip())
        if command == "dry-run":
            return 0
        deploy = wrangler(source_root, ["deploy", "--config", str(config_path)])
        sys.stdout.write(deploy.stdout)
        if deploy.returncode != 0:
            sys.stderr.write(deploy.stderr)
            return 1
        return 0
    except ProfileError as error:
        print("invalid profile: " + str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
