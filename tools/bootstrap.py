#!/usr/bin/env python3
"""Guided Cloudflare setup: create or reuse every instance resource by name.

Commands: up | down | plan --answers <file>.
Exit 2 for bad input, 1 for a Cloudflare or tool failure, 0 on success.
Cloudflare holds the state; resources are matched by name on every run.
"""
import getpass
import importlib.util
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
API = "https://api.cloudflare.com/client/v4"
PER_PAGE = 50
EMAIL = re.compile(r"^[^@\s,]+@[^@\s,]+\.[^@\s,]+$")
MISSING_ORG = ("Zero Trust is not set up for this account. Open Zero Trust at "
               "https://one.dash.cloudflare.com/ once to choose a team name, then rerun.")
FOREIGN_APP = ("An Access application for {hostname} already exists with the name \"{name}\", so bootstrap "
               "did not create it. Rename it to \"{hostname} bootstrap\" to let bootstrap manage it, or delete it, "
               "then rerun.")
HINTS = {
    "account_id": "Your account ID is on any domain's Overview page in https://dash.cloudflare.com/\n"
                  "Help: https://developers.cloudflare.com/fundamentals/account/find-account-and-zone-ids/",
    "zone": "The zone is a domain already active in this account; see the list at https://dash.cloudflare.com/",
    "hostname": "The hostname is a subdomain of the zone where the board will live. Pick one with no DNS record yet.",
    "emails": "The emails of the people who may sign in to the board through Cloudflare Access.",
    "token": "Create an API token at https://dash.cloudflare.com/profile/api-tokens with Create Custom Token.\n"
             "Help: https://developers.cloudflare.com/fundamentals/api/get-started/create-token/\n"
             "It needs these permissions:\n"
             "  Account: Workers Scripts Edit, Workers R2 Storage Edit, Access: Apps and Policies Edit,\n"
             "  Access: Service Tokens Edit, Access: Organizations, Identity Providers, and Groups Read\n"
             "  Zone: Workers Routes Edit, DNS Edit, Zone Read\n"
             "Set CLOUDFLARE_API_TOKEN to skip this prompt.",
}

_spec = importlib.util.spec_from_file_location("factory_deploy", Path(__file__).resolve().parent / "deploy.py")
deploy = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(deploy)
ProfileError = deploy.ProfileError


class CloudflareError(Exception):
    def __init__(self, message, status=None):
        super().__init__(message)
        self.status = status


def bad(field, message):
    return ProfileError(message + "\n" + HINTS[field])


def validate_answers(data):
    if not isinstance(data, dict):
        raise ProfileError("answers must be a JSON object")
    keys = ["version", "account_id", "zone", "hostname", "emails"]
    if sorted(data) != sorted(keys):
        raise ProfileError("answers need exactly the keys " + ", ".join(keys))
    if data["version"] != 1 or isinstance(data["version"], bool):
        raise ProfileError("version must be the number 1")
    if not isinstance(data["account_id"], str) or not deploy.ACCOUNT_ID.match(data["account_id"]):
        raise bad("account_id", "account_id must be 32 hexadecimal characters")
    for key in ("zone", "hostname"):
        if not isinstance(data[key], str):
            raise bad(key, key + " must be a string")
        try:
            deploy.https_origin("https://" + data[key], key)
        except ProfileError as error:
            raise bad(key, str(error))
    if not data["hostname"].endswith("." + data["zone"]):
        raise bad("hostname", "hostname " + data["hostname"] + " is not inside zone " + data["zone"])
    base = base_name(data["hostname"])
    if len(base) > 63:
        raise bad("hostname", "derived name " + base + " is longer than 63 characters; choose a shorter hostname")
    emails = data["emails"]
    if not isinstance(emails, list) or not emails:
        raise bad("emails", "emails must be a nonempty list")
    for email in emails:
        if not isinstance(email, str) or not EMAIL.match(email):
            raise bad("emails", "not an email address: " + json.dumps(email))
    return data


def device_label():
    return re.sub(r"[^a-z0-9]+", "-", socket.gethostname().lower())


def is_device_name(n, name):
    return re.fullmatch(re.escape(n["app"]) + " device [a-z0-9-]+", name) is not None


def base_name(hostname):
    return hostname.replace("-", "--").replace(".", "-")


def names(answers, device):
    base = base_name(answers["hostname"])
    app = answers["hostname"] + " bootstrap"
    return {
        "worker": base,
        "bucket": base,
        "app": app,
        "people_policy": app + " people",
        "device_policy": app + " device " + device,
        "token": app + " device " + device,
    }


def cloudflare(token):
    def api(method, path, body=None):
        data = None if body is None else json.dumps(body).encode("utf-8")
        request = urllib.request.Request(API + "/" + path, data=data, method=method, headers={
            "Authorization": "Bearer " + token,
            "Content-Type": "application/json",
            "User-Agent": "factory-bootstrap",
        })
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.loads(response.read().decode("utf-8"))
        except urllib.error.HTTPError as error:
            try:
                first = (json.loads(error.read().decode("utf-8")).get("errors") or [{}])[0]
            except ValueError:
                first = {}
            raise CloudflareError("{} {}: {} {}".format(
                method, path, first.get("code", error.code), first.get("message", error.reason)), error.code)
        except OSError as error:
            raise CloudflareError("{} {}: cannot reach api.cloudflare.com: {}".format(
                method, path, getattr(error, "reason", error)))
    return api


def api_list(api, path):
    items, page = [], 1
    separator = "&" if "?" in path else "?"
    while True:
        result = api("GET", "{}{}page={}&per_page={}".format(path, separator, page, PER_PAGE))["result"]
        items += result
        if len(result) < PER_PAGE:
            return items
        page += 1


def prompt_answers(saved):
    def ask(field, label, default):
        print(HINTS[field])
        value = input("{} [{}]: ".format(label, default) if default else label + ": ").strip()
        return value or default

    answers = {"version": 1}
    answers["account_id"] = ask("account_id", "Cloudflare account ID", saved.get("account_id", ""))
    answers["zone"] = ask("zone", "Zone, for example example.test", saved.get("zone", ""))
    answers["hostname"] = ask("hostname", "Factory hostname",
                              saved.get("hostname") or (answers["zone"] and "factory." + answers["zone"]))
    emails = ask("emails", "Allowed emails, comma separated", ", ".join(saved.get("emails", [])))
    answers["emails"] = [email.strip() for email in emails.split(",") if email.strip()]
    return answers


def read_json(path):
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except OSError as error:
        raise ProfileError("cannot read " + str(path) + ": " + error.strerror)
    except ValueError as error:
        raise ProfileError(str(path) + " is not valid JSON: " + str(error))


def plan(answers):
    n = names(answers, device_label())
    print("account  " + answers["account_id"])
    print("origin   https://" + answers["hostname"])
    print("worker   " + n["worker"])
    print("bucket   " + n["bucket"])
    print("app      " + n["app"])
    print("policy   " + n["people_policy"] + " (" + ", ".join(answers["emails"]) + ")")
    print("policy   " + n["device_policy"])
    print("token    " + n["token"])


def read_token():
    token = os.environ.get("CLOUDFLARE_API_TOKEN")
    if not token:
        print(HINTS["token"])
        token = getpass.getpass("Cloudflare API token: ")
    if not token:
        raise ProfileError("a Cloudflare API token is required")
    return token


def run_init(run, root, origin):
    """Point this device at the origin through factory.py init, from an empty directory."""
    with tempfile.TemporaryDirectory() as empty:
        connection = Path(empty) / "connection.json"
        connection.write_text(json.dumps({"version": 1, "api_url": origin}), encoding="utf-8")
        result = run([sys.executable, str(root / "skills" / "factory" / "runtime" / "factory.py"),
                      "init", "--connection", str(connection)], cwd=empty, capture_output=True, text=True)
    if result.returncode != 0:
        raise CloudflareError("factory.py init failed: " + result.stderr.strip())
    for line in result.stdout.splitlines():
        if line.startswith("factory home: "):
            return Path(line[len("factory home: "):])
    raise CloudflareError("factory.py init did not report the factory home")


def read_env(path):
    values = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        key, sep, value = line.partition("=")
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        if sep:
            values[key.strip()] = value
    return values


def write_env_credentials(home, client_id, client_secret):
    path = home / ".env"
    wanted = {"FACTORY_ACCESS_CLIENT_ID": client_id, "FACTORY_ACCESS_CLIENT_SECRET": client_secret}
    lines = []
    for line in path.read_text(encoding="utf-8").splitlines():
        key = line.partition("=")[0].strip()
        if key in wanted:
            line = key + "=" + wanted.pop(key)
        lines.append(line)
    lines += [key + "=" + value for key, value in wanted.items()]
    temporary = path.with_name(path.name + "." + str(os.getpid()) + ".tmp")
    with os.fdopen(os.open(str(temporary), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w",
                   encoding="utf-8") as stream:
        stream.write("\n".join(lines) + "\n")
    os.replace(temporary, path)


def ensure_bucket(api, account, name):
    try:
        api("GET", "accounts/{}/r2/buckets/{}".format(account, name))
        print("bucket " + name + ": kept")
    except CloudflareError as error:
        if error.status != 404:
            raise
        api("POST", "accounts/{}/r2/buckets".format(account), {"name": name})
        print("bucket " + name + ": created")


def access_issuer(api, account):
    try:
        domain = api("GET", "accounts/{}/access/organizations".format(account))["result"].get("auth_domain")
    except CloudflareError as error:
        if error.status != 404:
            raise
        domain = None
    if not domain:
        raise CloudflareError(MISSING_ORG)
    return "https://" + domain


def ensure_token(api, account, name, home):
    for token in api_list(api, "accounts/{}/access/service_tokens".format(account)):
        if token["name"] == name:
            if read_env(home / ".env").get("FACTORY_ACCESS_CLIENT_ID") != token["client_id"]:
                raise CloudflareError(
                    "Service token {} already exists and {} does not hold it. Delete it under Zero Trust "
                    "> Access > Service credentials, then rerun.".format(name, home / ".env"))
            print("token " + name + ": kept")
            return token["id"]
    token = api("POST", "accounts/{}/access/service_tokens".format(account), {"name": name})["result"]
    try:
        write_env_credentials(home, token["client_id"], token["client_secret"])
    except OSError as error:
        failed = "cannot write {}: {}.".format(home / ".env", error.strerror)
        try:
            api("DELETE", "accounts/{}/access/service_tokens/{}".format(account, token["id"]))
        except CloudflareError as delete_error:
            raise CloudflareError("{} Also could not delete the new service token {} ({}); delete it under Zero "
                                  "Trust > Access > Service credentials, then rerun up.".format(
                                      failed, name, delete_error))
        raise CloudflareError("{} Deleted the new service token {}; fix the file, then rerun up.".format(failed, name))
    print("token " + name + ": created, credentials written to " + str(home / ".env"))
    return token["id"]


def ensure_policy(api, account, name, decision, include):
    body = {"name": name, "decision": decision, "include": include}
    for policy in api_list(api, "accounts/{}/access/policies".format(account)):
        if policy["name"] == name:
            api("PUT", "accounts/{}/access/policies/{}".format(account, policy["id"]), body)
            print("policy " + name + ": updated")
            return policy["id"]
    policy = api("POST", "accounts/{}/access/policies".format(account), body)["result"]
    print("policy " + name + ": created")
    return policy["id"]


def app_body(hostname, n, policy_ids):
    return {"name": n["app"], "domain": hostname, "type": "self_hosted",
            "policies": [{"id": policy_id, "precedence": index + 1} for index, policy_id in enumerate(policy_ids)]}


def ensure_app(api, account, hostname, n, policy_ids):
    for app in api_list(api, "accounts/{}/access/apps".format(account)):
        if app.get("domain") == hostname:
            if app.get("name") != n["app"]:
                raise CloudflareError(FOREIGN_APP.format(hostname=hostname, name=app.get("name")))
            path = "accounts/{}/access/apps/{}".format(account, app["id"])
            app = api("GET", path)["result"]
            if not isinstance(app.get("policies"), list):
                raise CloudflareError("GET " + path + ": no policies list in the response; nothing was changed")
            current = [policy["id"] for policy in sorted(app["policies"], key=lambda policy: policy["precedence"])]
            missing = [policy_id for policy_id in policy_ids if policy_id not in current]
            if not missing:
                print("app " + hostname + ": kept")
                return app["aud"]
            app = api("PUT", path, app_body(hostname, n, current + missing))["result"]
            print("app " + hostname + ": updated")
            return app["aud"]
    app = api("POST", "accounts/{}/access/apps".format(account), app_body(hostname, n, policy_ids))["result"]
    print("app " + hostname + ": created")
    return app["aud"]


def write_if_changed(path, document):
    if not path.exists() or json.loads(path.read_text(encoding="utf-8")) != document:
        deploy.write_atomic(path, document)


def existing_profile(root, answers):
    path = root / "deploy" / "profile.json"
    if not path.exists():
        return None
    existing = read_json(path)
    for key, value in (("account_id", answers["account_id"]), ("origin", "https://" + answers["hostname"])):
        if existing.get(key) != value:
            raise ProfileError("{} has {} {}, but the answers say {}; edit or move it, then rerun".format(
                path, key, json.dumps(existing.get(key)), value))
    return existing


def write_profile(root, answers, n, existing, issuer, audience):
    path = root / "deploy" / "profile.json"
    profile = existing or {"version": 1, "account_id": answers["account_id"], "worker_name": n["worker"],
                           "bucket_name": n["bucket"], "origin": "https://" + answers["hostname"],
                           "title": "Factory", "image_origins": [], "agent_links": {}}
    profile = dict(profile, issuer=issuer, audience=audience)
    path.parent.mkdir(parents=True, exist_ok=True)
    write_if_changed(path, profile)
    return path


def up(answers, api, run, root):
    account, hostname = answers["account_id"], answers["hostname"]
    origin = "https://" + hostname
    n = names(answers, device_label())
    existing = existing_profile(root, answers)
    api("GET", "accounts/{}".format(account))
    if not api_list(api, "zones?name={}&account.id={}".format(answers["zone"], account)):
        raise CloudflareError("zone {} is not in account {}".format(answers["zone"], account))
    home = run_init(run, root, origin)
    ensure_bucket(api, account, existing["bucket_name"] if existing else n["bucket"])
    issuer = access_issuer(api, account)
    token_id = ensure_token(api, account, n["token"], home)
    people = ensure_policy(api, account, n["people_policy"], "allow",
                           [{"email": {"email": email}} for email in answers["emails"]])
    ensure_policy(api, account, n["device_policy"], "non_identity", [{"service_token": {"token_id": token_id}}])
    devices = sorted((policy["name"], policy["id"])
                     for policy in api_list(api, "accounts/{}/access/policies".format(account))
                     if policy["decision"] == "non_identity" and is_device_name(n, policy["name"]))
    audience = ensure_app(api, account, hostname, n, [people] + [pid for _, pid in devices])
    profile = write_profile(root, answers, n, existing, issuer, audience)
    print("profile " + str(profile))
    result = run([sys.executable, str(root / "tools" / "deploy.py"), "deploy", "--profile", str(profile)])
    if result.returncode != 0:
        raise CloudflareError("tools/deploy.py deploy failed; fix the cause above, then rerun up")
    print("Factory is live at " + origin)
    print("Next: open it in a browser and sign in as one of " + ", ".join(answers["emails"]) + ".")
    print("To add another device, run python3 tools/bootstrap.py up on that host.")


def down(answers, api, root, confirm=input):
    account, hostname = answers["account_id"], answers["hostname"]
    profile = read_json(root / "deploy" / "profile.json")
    n = names(answers, "")
    if confirm("Type {} to delete this instance: ".format(hostname)).strip() != hostname:
        raise ProfileError("confirmation did not match {}; nothing was deleted".format(hostname))
    for domain in api_list(api, "accounts/{}/workers/domains?hostname={}".format(account, hostname)):
        if domain["hostname"] == hostname:
            api("DELETE", "accounts/{}/workers/domains/{}".format(account, domain["id"]))
            print("custom domain " + hostname + ": deleted")
    try:
        api("DELETE", "accounts/{}/workers/scripts/{}?force=true".format(account, profile["worker_name"]))
        print("worker " + profile["worker_name"] + ": deleted")
    except CloudflareError as error:
        if error.status != 404:
            raise
    for app in api_list(api, "accounts/{}/access/apps".format(account)):
        if app.get("domain") == hostname:
            api("DELETE", "accounts/{}/access/apps/{}".format(account, app["id"]))
            print("app " + hostname + ": deleted")
    devices = set()
    for policy in api_list(api, "accounts/{}/access/policies".format(account)):
        device = policy["decision"] == "non_identity" and is_device_name(n, policy["name"])
        if device or (policy["name"] == n["people_policy"] and policy["decision"] == "allow"):
            api("DELETE", "accounts/{}/access/policies/{}".format(account, policy["id"]))
            print("policy " + policy["name"] + ": deleted")
            if device:
                devices.add(policy["name"])
    for token in api_list(api, "accounts/{}/access/service_tokens".format(account)):
        if token["name"] in devices:
            api("DELETE", "accounts/{}/access/service_tokens/{}".format(account, token["id"]))
            print("token " + token["name"] + ": deleted")
    bucket = profile["bucket_name"]
    try:
        api("DELETE", "accounts/{}/r2/buckets/{}".format(account, bucket))
        print("bucket " + bucket + ": deleted")
    except CloudflareError as error:
        print("bucket " + bucket + ": kept, Cloudflare said " + str(error))
    print("These local files still name the deleted instance: {}, {}, and the factory home "
          "config.md api_url and .env credentials, here and on every other device.".format(root / "deploy" / "profile.json",
                                                             root / "deploy" / "bootstrap.json"))


USAGE = "usage: python3 tools/bootstrap.py <up|down|plan --answers <file>>"


def main(argv=None):
    args = list(sys.argv[1:] if argv is None else argv)
    if not (args == ["up"] or args == ["down"] or (len(args) == 3 and args[:2] == ["plan", "--answers"])):
        print(USAGE, file=sys.stderr)
        return 2
    try:
        if args[0] == "plan":
            plan(validate_answers(read_json(args[2])))
            return 0
        saved_path = ROOT / "deploy" / "bootstrap.json"
        if args[0] == "up":
            answers = validate_answers(prompt_answers(read_json(saved_path) if saved_path.exists() else {}))
            saved_path.parent.mkdir(parents=True, exist_ok=True)
            write_if_changed(saved_path, answers)
            os.environ["CLOUDFLARE_API_TOKEN"] = read_token()
            up(answers, cloudflare(os.environ["CLOUDFLARE_API_TOKEN"]), subprocess.run, ROOT)
            return 0
        down(validate_answers(read_json(saved_path)), cloudflare(read_token()), ROOT, input)
        return 0
    except ProfileError as error:
        print("invalid input: " + str(error), file=sys.stderr)
        return 2
    except (KeyboardInterrupt, EOFError):
        print("cancelled", file=sys.stderr)
        return 2
    except CloudflareError as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
