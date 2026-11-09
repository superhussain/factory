"""Guided Cloudflare setup against an in-memory Cloudflare API and a fake subprocess runner.
Synthetic values only; nothing here reaches the network or the device's real factory home."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent.parent
FIXTURE = ROOT / "tests" / "fixtures" / "bootstrap-valid.json"

spec = importlib.util.spec_from_file_location("factory_bootstrap", ROOT / "tools" / "bootstrap.py")
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)

ACCOUNT = "00000000000000000000000000000000"
API_TOKEN = "synthetic-api-token"
SECRET = "synthetic-client-secret"
ANSWERS = [ACCOUNT, "example.test", "factory.example.test", "ada@example.test, grace@example.test"]


class FakeCloudflare:
    """Enough of the v4 API for bootstrap: R2 buckets, Access, service tokens and Workers."""

    def __init__(self, org="example.cloudflareaccess.com"):
        self.org = org
        self.calls = []
        self.buckets = {}
        self.items = {"service_tokens": {}, "policies": {}, "apps": {}, "domains": {}}
        self.scripts = set()
        self.next_id = 0
        self.fail = []

    def new_id(self):
        self.next_id += 1
        return "id-" + str(self.next_id)

    def error(self, method, path, status, code, message):
        raise bootstrap.CloudflareError("{} {}: {} {}".format(method, path, code, message), status)

    def __call__(self, method, path, body=None):
        self.calls.append((method, path, body))
        route, _, query = path.partition("?")
        params = dict(pair.split("=", 1) for pair in query.split("&") if pair)
        parts = route.split("/")
        if parts == ["accounts", ACCOUNT] and method == "GET":
            return {"success": True, "result": {"id": ACCOUNT}}
        if parts == ["zones"]:
            zones = [{"name": "example.test"}] if params.get("name") == "example.test" else []
            return {"success": True, "result": zones}
        if parts[:2] != ["accounts", ACCOUNT]:
            self.error(method, path, 403, 9109, "Unauthorized to access requested resource")
        rest = parts[2:]
        kind = rest[1]
        if (method, kind) in self.fail:
            self.fail.remove((method, kind))
            self.error(method, path, 500, 10000, "synthetic failure")
        if rest[:2] == ["r2", "buckets"]:
            if len(rest) == 2 and method == "POST":
                self.buckets[body["name"]] = 0
                return {"success": True, "result": {"name": body["name"]}}
            name = rest[2]
            if name not in self.buckets:
                self.error(method, path, 404, 10006, "The specified bucket does not exist.")
            if method == "DELETE":
                if self.buckets[name]:
                    self.error(method, path, 409, 10008, "The bucket you tried to delete is not empty.")
                del self.buckets[name]
            return {"success": True, "result": {"name": name}}
        if rest == ["access", "organizations"]:
            if not self.org:
                self.error(method, path, 404, 12130, "access.api.error.not_found")
            return {"success": True, "result": {"auth_domain": self.org}}
        if rest == ["workers", "scripts", rest[-1]] and method == "DELETE":
            if rest[-1] not in self.scripts:
                self.error(method, path, 404, 10007, "This Worker does not exist on your account.")
            self.scripts.discard(rest[-1])
            return {"success": True, "result": None}
        store = self.items[kind]
        if method == "GET" and len(rest) == 2:
            found = [{k: v for k, v in item.items() if k != "client_secret"} for item in store.values()
                     if kind != "domains" or item["hostname"] == params.get("hostname")]
            return {"success": True, "result": found}
        if method == "POST":
            item = dict(body, id=self.new_id())
            if kind == "service_tokens":
                item.update(client_id="client-" + item["id"] + ".access", client_secret=SECRET)
            if kind == "apps":
                item["aud"] = "aud-" + item["id"]
            store[item["id"]] = item
            return {"success": True, "result": dict(item)}
        item_id = rest[-1]
        if item_id not in store:
            self.error(method, path, 404, 12130, "not found")
        if method == "GET":
            return {"success": True, "result": dict(store[item_id])}
        if method == "PUT":
            store[item_id] = dict(store[item_id], **body)
            return {"success": True, "result": dict(store[item_id])}
        del store[item_id]
        return {"success": True, "result": {"id": item_id}}

    def methods(self, since=0):
        return [method for method, _, _ in self.calls[since:]]


class FakeRun:
    """Stands in for subprocess.run: factory.py init seeds the home, deploy.py attaches the domain."""

    def __init__(self, home, fake):
        self.home, self.fake, self.calls = home, fake, []

    def __call__(self, args, **kwargs):
        self.calls.append((args, kwargs))
        if "init" in args:
            connection = json.loads(Path(args[args.index("--connection") + 1]).read_text())
            self.connection = connection
            self.home.mkdir(parents=True, exist_ok=True)
            env = self.home / ".env"
            if not env.exists():
                env.write_text("FACTORY_ACCESS_CLIENT_ID=\nFACTORY_ACCESS_CLIENT_SECRET=\n")
                os.chmod(env, 0o600)
            return subprocess.CompletedProcess(args, 0, "factory home: " + str(self.home) + "\n", "")
        profile = json.loads(Path(args[args.index("--profile") + 1]).read_text())
        self.fake.scripts.add(profile["worker_name"])
        if not self.fake.items["domains"]:
            self.fake.items["domains"]["dom-1"] = {"id": "dom-1", "hostname": "factory.example.test",
                                                   "service": profile["worker_name"]}
        return subprocess.CompletedProcess(args, 0, "", "")


class InputTest(unittest.TestCase):
    def plan(self, document):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
            json.dump(document, handle)
        self.addCleanup(os.unlink, handle.name)
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = bootstrap.main(["plan", "--answers", handle.name])
        return code, out.getvalue(), err.getvalue()

    def test_plan_prints_resource_names_offline(self):
        result = subprocess.run(["python3", str(ROOT / "tools" / "bootstrap.py"), "plan", "--answers",
                                 str(FIXTURE)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("worker   factory-example-test", result.stdout)
        self.assertIn("app      factory.example.test bootstrap", result.stdout)
        self.assertIn("policy   factory.example.test bootstrap people (ada@example.test, grace@example.test)",
                      result.stdout)

    def test_bad_answers_exit_2(self):
        base = json.loads(FIXTURE.read_text())
        cases = [
            ("hostname outside zone", {"hostname": "factory.other.test"}, "is not inside zone"),
            ("hostname equal to zone", {"hostname": "example.test"}, "is not inside zone"),
            ("bad email", {"emails": ["ada.example.test"]}, "not an email address"),
            ("no emails", {"emails": []}, "emails must be a nonempty list"),
            ("short account", {"account_id": "0" * 31}, "account_id must be 32 hexadecimal"),
            ("uppercase account", {"account_id": "A" * 32}, "account_id must be 32 hexadecimal"),
            ("bad zone", {"zone": "exa mple.test"}, "zone"),
            ("version", {"version": 2}, "version must be the number 1"),
            ("extra key", {"token": "x"}, "answers need exactly the keys"),
            ("long name", {"hostname": "a" * 52 + ".example.test"}, "is longer than 63 characters"),
        ]
        for name, change, message in cases:
            with self.subTest(name):
                code, out, err = self.plan(dict(base, **change))
                self.assertEqual(code, 2, err)
                self.assertEqual(out, "")
                self.assertIn(message, err)

    def test_usage_exits_2(self):
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(bootstrap.main(["apply"]), 2)
            self.assertEqual(bootstrap.main(["plan"]), 2)

    def test_names_and_device_label(self):
        with mock.patch("socket.gethostname", return_value="Ada's MacBook.local"):
            self.assertEqual(bootstrap.device_label(), "ada-s-macbook-local")
        n = bootstrap.names({"hostname": "factory.example.test"}, "laptop")
        self.assertEqual(n, {"worker": "factory-example-test", "bucket": "factory-example-test",
                             "app": "factory.example.test bootstrap",
                             "people_policy": "factory.example.test bootstrap people",
                             "device_policy": "factory.example.test bootstrap device laptop",
                             "token": "factory.example.test bootstrap device laptop"})

    def test_dot_and_dash_hostnames_get_distinct_names(self):
        dot = bootstrap.names({"hostname": "a.b.example.test"}, "laptop")
        dash = bootstrap.names({"hostname": "a-b.example.test"}, "laptop")
        self.assertEqual(dot["worker"], "a-b-example-test")
        self.assertEqual(dash["worker"], "a--b-example-test")
        self.assertEqual(dot["device_policy"], "a.b.example.test bootstrap device laptop")
        for key in dot:
            self.assertNotEqual(dot[key], dash[key], key)
        self.assertFalse(bootstrap.is_device_name(dot, dash["token"]))
        self.assertFalse(bootstrap.is_device_name(dash, dot["token"]))


class Instance(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.tmp = Path(temporary.name)
        self.root = self.tmp / "checkout"
        self.root.mkdir()
        self.home = self.tmp / "home"
        self.fake = FakeCloudflare()
        self.run = FakeRun(self.home, self.fake)

    def main(self, command, inputs, host="Laptop", home=None, real_api=False, token_env=True):
        run = self.run if home is None else FakeRun(home, self.fake)
        out, err = io.StringIO(), io.StringIO()
        api = bootstrap.cloudflare if real_api else (lambda token: self.fake)
        answers = iter(inputs)

        def terminal(prompt):
            print(prompt, end="")
            answer = next(answers)
            if isinstance(answer, type) and issubclass(answer, BaseException):
                raise answer
            print(answer)
            return answer

        with mock.patch.object(bootstrap, "ROOT", self.root), \
                mock.patch.object(bootstrap, "cloudflare", api), \
                mock.patch("subprocess.run", run), \
                mock.patch("builtins.input", terminal), \
                mock.patch("getpass.getpass", lambda prompt: print(prompt) or API_TOKEN), \
                mock.patch("socket.gethostname", return_value=host), \
                mock.patch.dict(os.environ, {"CLOUDFLARE_API_TOKEN": API_TOKEN}), \
                contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            if not token_env:
                del os.environ["CLOUDFLARE_API_TOKEN"]
            code = bootstrap.main([command])
        return code, out.getvalue(), err.getvalue()

    def up(self, inputs=ANSWERS, **kwargs):
        return self.main("up", inputs, **kwargs)

    def files(self):
        return {path: path.read_bytes() for base in (self.root, self.home)
                for path in sorted(base.rglob("*")) if path.is_file()}

    def env(self):
        return bootstrap.read_env(self.home / ".env")

    def policies(self):
        return {item["name"]: item for item in self.fake.items["policies"].values()}


class FreshUpTest(Instance):
    def test_fresh_up_creates_everything_and_deploys(self):
        code, out, err = self.up()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.fake.buckets, {"factory-example-test": 0})
        [token] = self.fake.items["service_tokens"].values()
        self.assertEqual(token["name"], "factory.example.test bootstrap device laptop")
        self.assertEqual(self.env()["FACTORY_ACCESS_CLIENT_ID"], token["client_id"])
        self.assertEqual(self.env()["FACTORY_ACCESS_CLIENT_SECRET"], SECRET)
        if os.name == "posix":
            self.assertEqual((self.home / ".env").stat().st_mode & 0o777, 0o600)
        policies = self.policies()
        people = policies["factory.example.test bootstrap people"]
        device = policies["factory.example.test bootstrap device laptop"]
        self.assertEqual(people["decision"], "allow")
        self.assertEqual(people["include"], [{"email": {"email": "ada@example.test"}},
                                             {"email": {"email": "grace@example.test"}}])
        self.assertEqual(device["decision"], "non_identity")
        self.assertEqual(device["include"], [{"service_token": {"token_id": token["id"]}}])
        [app] = self.fake.items["apps"].values()
        self.assertEqual((app["domain"], app["type"]), ("factory.example.test", "self_hosted"))
        self.assertEqual(app["policies"], [{"id": people["id"], "precedence": 1},
                                           {"id": device["id"], "precedence": 2}])
        profile = json.loads((self.root / "deploy" / "profile.json").read_text())
        self.assertEqual(profile, {
            "version": 1, "account_id": ACCOUNT, "worker_name": "factory-example-test",
            "bucket_name": "factory-example-test", "origin": "https://factory.example.test",
            "title": "Factory", "issuer": "https://example.cloudflareaccess.com",
            "audience": app["aud"], "image_origins": [], "agent_links": {}})
        saved = json.loads((self.root / "deploy" / "bootstrap.json").read_text())
        self.assertEqual(saved, json.loads(FIXTURE.read_text()))
        self.assertIn("Factory is live at https://factory.example.test", out)

    def test_first_run_shows_where_to_find_each_value(self):
        code, out, err = self.up(inputs=[ACCOUNT, "example.test", "", "ada@example.test"], token_env=False)
        self.assertEqual(code, 0, err)
        expected = [
            "Your account ID is on any domain's Overview page in https://dash.cloudflare.com/",
            "https://developers.cloudflare.com/fundamentals/account/find-account-and-zone-ids/",
            "Cloudflare account ID: " + ACCOUNT,
            "The zone is a domain already active in this account; see the list at https://dash.cloudflare.com/",
            "Zone, for example example.test: example.test",
            "The hostname is a subdomain of the zone where the board will live.",
            "Factory hostname [factory.example.test]: \n",
            "The emails of the people who may sign in to the board through Cloudflare Access.",
            "Allowed emails, comma separated: ada@example.test",
            "Create an API token at https://dash.cloudflare.com/profile/api-tokens",
            "https://developers.cloudflare.com/fundamentals/api/get-started/create-token/",
            "Account: Workers Scripts Edit, Workers R2 Storage Edit, Access: Apps and Policies Edit,",
            "Access: Service Tokens Edit, Access: Organizations, Identity Providers, and Groups Read",
            "Zone: Workers Routes Edit, DNS Edit, Zone Read",
            "Set CLOUDFLARE_API_TOKEN to skip this prompt.",
            "Cloudflare API token: ",
        ]
        positions = [out.index(text) for text in expected]
        self.assertEqual(positions, sorted(positions), "each hint comes right before its prompt")
        self.assertNotIn(API_TOKEN, out + err)

    def test_hostname_defaults_to_factory_dot_zone(self):
        self.up(inputs=[ACCOUNT, "example.test", "", "ada@example.test"])
        saved = json.loads((self.root / "deploy" / "bootstrap.json").read_text())
        self.assertEqual(saved["hostname"], "factory.example.test")

    def test_token_hint_is_skipped_when_the_env_holds_the_token(self):
        _, out, _ = self.up()
        self.assertNotIn("api-tokens", out)

    def test_bad_answer_repeats_where_to_find_it(self):
        code, _, err = self.up(inputs=["not-an-account", "example.test", "", "ada@example.test"])
        self.assertEqual(code, 2)
        self.assertIn("account_id must be 32 hexadecimal characters", err)
        self.assertIn("https://developers.cloudflare.com/fundamentals/account/find-account-and-zone-ids/", err)

    def test_init_runs_first_from_an_empty_directory_and_deploy_last(self):
        self.up()
        (init, init_kwargs), (deploy, _) = self.run.calls
        self.assertEqual(init[1:], [str(self.root / "skills" / "factory" / "runtime" / "factory.py"),
                                    "init", "--connection", init[-1]])
        self.assertEqual(self.run.connection, {"version": 1, "api_url": "https://factory.example.test"})
        self.assertNotEqual(Path(init_kwargs["cwd"]), self.root)
        self.assertEqual(deploy[1:], [str(self.root / "tools" / "deploy.py"), "deploy", "--profile",
                                      str(self.root / "deploy" / "profile.json")])
        first_write = next(i for i, (method, _, _) in enumerate(self.fake.calls) if method != "GET")
        self.assertEqual(self.fake.calls[first_write][1], "accounts/" + ACCOUNT + "/r2/buckets")

    def test_env_is_written_before_any_policy_or_app(self):
        self.up()
        posts = [path.split("/")[-1] for method, path, _ in self.fake.calls if method == "POST"]
        self.assertEqual(posts, ["buckets", "service_tokens", "policies", "policies", "apps"])

    def test_existing_profile_keeps_names_and_extra_keys(self):
        (self.root / "deploy").mkdir()
        existing = {"version": 1, "account_id": ACCOUNT, "worker_name": "older-worker",
                    "bucket_name": "older-bucket", "origin": "https://factory.example.test",
                    "title": "Mine", "issuer": "https://old.cloudflareaccess.com", "audience": "old",
                    "image_origins": [], "agent_links": {}, "jira_origin": "https://jira.example.test"}
        (self.root / "deploy" / "profile.json").write_text(json.dumps(existing))
        code, _, err = self.up()
        self.assertEqual(code, 0, err)
        profile = json.loads((self.root / "deploy" / "profile.json").read_text())
        [app] = self.fake.items["apps"].values()
        self.assertEqual(profile, dict(existing, issuer="https://example.cloudflareaccess.com",
                                       audience=app["aud"]))
        self.assertEqual(self.fake.buckets, {"older-bucket": 0})

    def test_existing_profile_for_another_origin_exits_2(self):
        (self.root / "deploy").mkdir()
        (self.root / "deploy" / "profile.json").write_text(json.dumps(
            {"account_id": ACCOUNT, "origin": "https://other.example.test"}))
        code, _, err = self.up()
        self.assertEqual(code, 2)
        self.assertIn("origin", err)
        self.assertIn("edit or move", err)
        self.assertNotIn("POST", self.fake.methods())
        self.assertEqual(self.run.calls, [])

    def test_network_failure_exits_1_with_one_line(self):
        for error, reason in ((bootstrap.urllib.error.URLError("Name or service not known"),
                               "Name or service not known"),
                              (socket.timeout("timed out"), "timed out")):
            with self.subTest(reason), mock.patch("urllib.request.urlopen", side_effect=error) as urlopen:
                code, _, err = self.up(real_api=True)
                self.assertEqual(code, 1)
                self.assertEqual(err, "GET accounts/" + ACCOUNT + ": cannot reach api.cloudflare.com: "
                                 + reason + "\n")
                self.assertEqual(urlopen.call_args[1], {"timeout": 30})

    def test_cancel_at_a_prompt_exits_cleanly(self):
        for interrupt in (KeyboardInterrupt, EOFError):
            with self.subTest(interrupt.__name__):
                code, out, err = self.up(inputs=[ACCOUNT, interrupt])
                self.assertEqual(code, 2)
                self.assertEqual(err, "cancelled\n")
                self.assertFalse((self.root / "deploy").exists())

    def test_client_sends_user_agent_and_bearer_token(self):
        seen = []

        class Response(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *args):
                return False

        def urlopen(request, timeout):
            self.assertEqual(timeout, 30)
            seen.append(request)
            return Response(b'{"success": true, "result": []}')

        with mock.patch("urllib.request.urlopen", urlopen):
            bootstrap.cloudflare(API_TOKEN)("GET", "accounts/" + ACCOUNT)
        self.assertEqual(seen[0].full_url, "https://api.cloudflare.com/client/v4/accounts/" + ACCOUNT)
        self.assertEqual(seen[0].get_header("User-agent"), "factory-bootstrap")
        self.assertEqual(seen[0].get_header("Authorization"), "Bearer " + API_TOKEN)


class RerunTest(Instance):
    def test_second_up_makes_no_post_and_changes_no_file(self):
        self.assertEqual(self.up()[0], 0)
        before, since = self.files(), len(self.fake.calls)
        code, out, err = self.up(inputs=["", "", "", ""])
        self.assertEqual(code, 0, err)
        self.assertNotIn("POST", self.fake.methods(since))
        self.assertEqual(self.files(), before)
        self.assertIn("token factory.example.test bootstrap device laptop: kept", out)

    def test_quoted_env_client_id_still_matches(self):
        self.up()
        env = self.home / ".env"
        client_id = self.env()["FACTORY_ACCESS_CLIENT_ID"]
        env.write_text('FACTORY_ACCESS_CLIENT_ID="' + client_id + '"\nFACTORY_ACCESS_CLIENT_SECRET=x\n')
        code, out, err = self.up(inputs=["", "", "", ""])
        self.assertEqual(code, 0, err)
        self.assertIn("token factory.example.test bootstrap device laptop: kept", out)

    def test_foreign_names_sharing_the_prefix_are_not_attached(self):
        self.fake.items["policies"]["foreign"] = {"id": "foreign", "name": "factory.example.test bootstrap device evil",
                                                  "decision": "allow", "include": []}
        code, _, err = self.up()
        self.assertEqual(code, 0, err)
        [app] = self.fake.items["apps"].values()
        self.assertNotIn("foreign", [p["id"] for p in app["policies"]])

    def test_second_device_adds_its_policy_to_the_app(self):
        self.up()
        code, _, err = self.up(inputs=["", "", "", ""], host="Desk Top", home=self.tmp / "home2")
        self.assertEqual(code, 0, err)
        policies = self.policies()
        [app] = self.fake.items["apps"].values()
        self.assertEqual([p["id"] for p in app["policies"]], [
            policies["factory.example.test bootstrap people"]["id"],
            policies["factory.example.test bootstrap device laptop"]["id"],
            policies["factory.example.test bootstrap device desk-top"]["id"]])

    def test_rerun_keeps_hand_added_policies(self):
        self.up()
        [app] = self.fake.items["apps"].values()
        self.fake.items["policies"]["block"] = {"id": "block", "name": "Block contractors", "decision": "deny",
                                                "include": [{"email_domain": {"domain": "contractor.test"}}]}
        app["policies"] = [{"id": "block", "precedence": 1}] + [
            dict(policy, precedence=policy["precedence"] + 1) for policy in app["policies"]]
        since = len(self.fake.calls)
        code, out, err = self.up(inputs=["", "", "", ""])
        self.assertEqual(code, 0, err)
        self.assertIn("app factory.example.test: kept", out)
        self.assertNotIn("PUT", [m for m, path, _ in self.fake.calls[since:] if "/apps/" in path])
        code, _, err = self.up(inputs=["", "", "", ""], host="Desk Top", home=self.tmp / "home2")
        self.assertEqual(code, 0, err)
        policies = self.policies()
        [app] = self.fake.items["apps"].values()
        self.assertEqual(app["policies"], [
            {"id": "block", "precedence": 1},
            {"id": policies["factory.example.test bootstrap people"]["id"], "precedence": 2},
            {"id": policies["factory.example.test bootstrap device laptop"]["id"], "precedence": 3},
            {"id": policies["factory.example.test bootstrap device desk-top"]["id"], "precedence": 4}])

    def test_up_refuses_an_app_bootstrap_did_not_create(self):
        self.fake.items["apps"]["mine"] = {"id": "mine", "name": "Factory", "domain": "factory.example.test",
                                           "type": "self_hosted", "aud": "aud-mine", "policies": []}
        code, _, err = self.up()
        self.assertEqual(code, 1)
        self.assertEqual(err, "An Access application for factory.example.test already exists with the name "
                         "\"Factory\", so bootstrap did not create it. Rename it to \"factory.example.test "
                         "bootstrap\" to let bootstrap manage it, or delete it, then rerun.\n")
        self.assertEqual(self.fake.items["apps"]["mine"]["name"], "Factory")
        self.assertNotIn("PUT", [m for m, path, _ in self.fake.calls if "/apps/" in path])
        self.assertEqual(len(self.run.calls), 1, "no deploy")

    def test_app_without_a_policies_list_is_not_changed(self):
        self.up()
        [app] = self.fake.items["apps"].values()
        del app["policies"]
        since = len(self.fake.calls)
        code, _, err = self.up(inputs=["", "", "", ""])
        self.assertEqual(code, 1)
        self.assertEqual(err, "GET accounts/" + ACCOUNT + "/access/apps/" + app["id"]
                         + ": no policies list in the response; nothing was changed\n")
        self.assertNotIn("PUT", [m for m, path, _ in self.fake.calls[since:] if "/apps/" in path])


class TokenConflictTest(Instance):
    def test_existing_token_without_matching_env_exits_1_and_keeps_env(self):
        self.fake.items["service_tokens"]["old"] = {
            "id": "old", "name": "factory.example.test bootstrap device laptop", "client_id": "client-old.access"}
        code, out, err = self.up()
        self.assertEqual(code, 1)
        env = self.home / ".env"
        self.assertEqual(err, "Service token factory.example.test bootstrap device laptop already exists and "
                         + str(env) + " does not hold it. Delete it under Zero Trust > Access > "
                         "Service credentials, then rerun.\n")
        self.assertEqual(env.read_text(), "FACTORY_ACCESS_CLIENT_ID=\nFACTORY_ACCESS_CLIENT_SECRET=\n")
        self.assertEqual(self.fake.methods().count("POST"), 1, "only the bucket was created")
        self.assertEqual(len(self.run.calls), 1, "no deploy")

    def test_missing_zero_trust_org_exits_1_before_the_token(self):
        self.fake.org = None
        code, _, err = self.up()
        self.assertEqual(code, 1)
        self.assertEqual(err, "Zero Trust is not set up for this account. Open Zero Trust at "
                         "https://one.dash.cloudflare.com/ once to choose a team name, then rerun.\n")
        self.assertEqual(self.fake.items["service_tokens"], {})


class SecretTest(Instance):
    def test_secret_never_reaches_output(self):
        outputs = []
        for inputs in (ANSWERS, ["", "", "", ""]):
            code, out, err = self.up(inputs=inputs)
            self.assertEqual(code, 0, err)
            outputs += [out, err]
        for text in outputs:
            self.assertNotIn(SECRET, text)
            self.assertNotIn(API_TOKEN, text)
        self.assertNotIn(SECRET, (self.root / "deploy" / "profile.json").read_text())
        self.assertNotIn(SECRET, (self.root / "deploy" / "bootstrap.json").read_text())

    def test_api_error_prints_method_path_code_and_message_only(self):
        body = json.dumps({"errors": [{"code": 10000, "message": "Authentication error"}]}).encode()
        error = bootstrap.urllib.error.HTTPError("https://api.cloudflare.com/client/v4/x", 403, "Forbidden",
                                                 {}, io.BytesIO(body))
        with mock.patch("urllib.request.urlopen", side_effect=error):
            with self.assertRaises(bootstrap.CloudflareError) as raised:
                bootstrap.cloudflare(API_TOKEN)("GET", "accounts/" + ACCOUNT)
        self.assertEqual(str(raised.exception), "GET accounts/" + ACCOUNT + ": 10000 Authentication error")


class DownTest(Instance):
    def down(self, typed):
        return self.main("down", [typed])

    def test_wrong_confirmation_exits_2_and_deletes_nothing(self):
        self.up()
        since = len(self.fake.calls)
        code, _, err = self.down("factory.example")
        self.assertEqual(code, 2)
        self.assertIn("nothing was deleted", err)
        self.assertNotIn("DELETE", self.fake.methods(since))
        self.assertIn("factory-example-test", self.fake.buckets)

    def test_down_deletes_in_order_and_keeps_a_nonempty_bucket(self):
        self.up()
        self.fake.buckets["factory-example-test"] = 3
        files = self.files()
        since = len(self.fake.calls)
        code, out, err = self.down("factory.example.test")
        self.assertEqual(code, 0, err)
        deletes = [path.split("?")[0].split("/")[-2] for method, path, _ in self.fake.calls[since:]
                   if method == "DELETE"]
        self.assertEqual(deletes, ["domains", "scripts", "apps", "policies", "policies",
                                   "service_tokens", "buckets"])
        self.assertEqual(self.fake.items, {"service_tokens": {}, "policies": {}, "apps": {}, "domains": {}})
        self.assertEqual(self.fake.scripts, set())
        self.assertEqual(self.fake.buckets, {"factory-example-test": 3})
        self.assertIn("bucket factory-example-test: kept, Cloudflare said DELETE accounts/" + ACCOUNT
                      + "/r2/buckets/factory-example-test: 10008 The bucket you tried to delete is not empty.",
                      out)
        self.assertIn("still name the deleted instance", out)
        self.assertEqual(self.files(), files, "local files stay")
        self.assertNotIn(SECRET, out + err)

    def test_down_deletes_an_empty_bucket(self):
        self.up()
        code, out, err = self.down("factory.example.test")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.fake.buckets, {})
        self.assertIn("bucket factory-example-test: deleted", out)

    def test_down_leaves_foreign_names_sharing_the_prefix(self):
        self.up()
        self.fake.items["policies"]["foreign"] = {"id": "foreign", "name": "factory.example.test bootstrap device evil",
                                                  "decision": "allow", "include": []}
        self.fake.items["service_tokens"]["stray"] = {"id": "stray", "name": "factory.example.test bootstrapped device gone",
                                                      "client_id": "client-stray.access"}
        code, _, err = self.down("factory.example.test")
        self.assertEqual(code, 0, err)
        self.assertEqual(list(self.fake.items["policies"]), ["foreign"])
        self.assertEqual(list(self.fake.items["service_tokens"]), ["stray"])


if __name__ == "__main__":
    unittest.main()
