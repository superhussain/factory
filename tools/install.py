"""Copy the self-contained factory skill. Python 3.8+, stdlib only."""

import argparse
import errno
import hashlib
import importlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path


REQUIRED = (
    "SKILL.md", "runtime/factory.py", "runtime/paseo_stalls.py", "runtime/pr_screenshots.py",
    "templates/config.md", "templates/project.md", "references/execution.md", "references/integrations.md",
    "references/pickup.md", "references/visuals.md", "references/chat-decisions.md",
    "references/project-settings.md", "references/dashboard.md", "references/board.md", "references/needs.md", "references/schedules.md",
    "references/walkthrough.html", "schedules/explore.md", "schedules/resume.md", "schedules/retro.md",
) + tuple("references/commands/" + name + ".md" for name in (
    "feature", "next", "idea", "deny-notes", "babysit", "approve", "approve-spec", "rework", "recover", "build", "init", "catch-up", "handoff",
    "resume", "retro")) + tuple("agents/factory-" + name + ".md" for name in (
    "approver", "builder", "explorer", "review-code", "review-security", "review-ui", "review-ux", "spec-writer",
    "retro"))

LEGACY = "huss-factory"
BINARY = "runtime/factory.exe" if os.name == "nt" else "runtime/factory"
LOCK = ".build.lock"
PROFILE = "host"
KEEP = 3  # binaries kept in the cache, newest first
LOCK_WAIT = 1800  # seconds a Windows install waits for another one's build
# A literal include_str! or include_bytes! path, relative to the .rs file that holds it.
EMBED = re.compile(r'include_(?:str|bytes)!\(\s*"([^"]+)"\s*\)')


def fail(message):
    print("install.py: " + message, file=sys.stderr)
    raise SystemExit(2)


def rust_binary(checkout):
    """The host-profile binary cargo builds from checkout's rust/ folder, or the cached one built from the
    same inputs, or None when cargo is missing, the build fails, FACTORY_RUNTIME is python or the
    checkout has no Rust workspace."""
    manifest = checkout / "rust" / "Cargo.toml"
    if os.environ.get("FACTORY_RUNTIME") == "python" or not manifest.is_file():
        return None
    cargo = shutil.which("cargo")
    if cargo is None:
        # rustup installs into ~/.cargo/bin, which launchd and non-login shells leave off PATH.
        home_cargo = Path.home() / ".cargo" / "bin" / ("cargo.exe" if os.name == "nt" else "cargo")
        if home_cargo.is_file() and os.access(home_cargo, os.X_OK):
            cargo = str(home_cargo)
    if cargo is None:
        print("install: cargo not found, keeping the Python runtime", file=sys.stderr)
        return None
    try:
        if os.name == "nt" and not os.environ.get("LOCALAPPDATA"):
            raise OSError("LOCALAPPDATA is not set")
        name = Path(BINARY).name
        cache = (Path(os.environ["LOCALAPPDATA"], "huss-skills", "factory") if os.name == "nt"
                 else Path.home() / ".cache" / "huss-skills" / "factory")
        # A per-user target folder keeps dependencies compiled across catch-up's fresh source worktrees.
        target = Path(os.environ.get("CARGO_TARGET_DIR") or cache / "cargo-target")
        files = build_inputs(checkout)
        key = hashlib.sha256()
        for rel in sorted(files):
            key.update(rel.encode() + b"\0" + hashlib.sha256(files[rel]).digest())
        # rustc -vV prints the pinned toolchain's version and its host target triple; cwd picks the pin.
        rustc = Path(cargo).with_name("rustc.exe" if os.name == "nt" else "rustc")
        key.update(run([str(rustc), "-vV"], checkout / "rust").encode() + b"\0" + PROFILE.encode())
        # Flags and the user's cargo config change the binary without changing any synced file.
        for flag in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"):
            key.update(b"\0" + flag.encode() + b"=" + os.environ.get(flag, "").encode())
        cargo_home = Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")
        for config in ("config.toml", "config"):
            if (cargo_home / config).is_file():
                key.update(b"\0" + config.encode() + b"\0" + (cargo_home / config).read_bytes())
        key = key.hexdigest()
        stored = private_dir(cache / "bin") / key
        if (stored / name).is_file() and (stored / (name + ".sha256")).is_file():
            binary = (stored / name).read_bytes()
            if hashlib.sha256(binary).hexdigest() == (stored / (name + ".sha256")).read_text().strip():
                print("install: Rust binary from cache " + key[:8], file=sys.stderr)
                return binary
        os.makedirs(target, mode=0o700, exist_ok=True)
        # One stable source path keeps cargo's incremental artifacts valid from one checkout to the next.
        src = private_dir(cache / "build-src")
        with open(src / LOCK, "a") as lock:
            hold(lock)
            sync(files, src)
            start = time.monotonic()
            # cwd is the workspace so rustup reads its rust-toolchain.toml.
            run([cargo, "build", "--profile", PROFILE, "--locked", "--manifest-path", str(src / "rust" / "Cargo.toml")],
                src / "rust", dict(os.environ, CARGO_TARGET_DIR=str(target)))
            binary = (target / PROFILE / name).read_bytes()
            stored.mkdir(exist_ok=True)
            # The sha256 goes last, so a reader never matches a half-written binary.
            write_atomic(stored / name, binary)
            write_atomic(stored / (name + ".sha256"), (hashlib.sha256(binary).hexdigest() + "\n").encode())
            os.utime(stored)
            others = [path for path in stored.parent.iterdir() if path != stored]
            for old in sorted(others, key=lambda path: path.stat().st_mtime_ns, reverse=True)[KEEP - 1:]:
                shutil.rmtree(old)
        print("install: Rust binary built in " + str(round(time.monotonic() - start)) + " s", file=sys.stderr)
        return binary
    except (OSError, ValueError) as e:
        print("install: Rust build failed, keeping the Python runtime (" + str(e) + ")", file=sys.stderr)
        return None


def run(args, cwd, env=None):
    """args' stdout, or OSError carrying the last line it wrote to stderr."""
    r = subprocess.run(args, cwd=str(cwd), env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                       errors="replace")
    if r.returncode:
        lines = [line for line in r.stderr.splitlines() if line.strip()] or ["exit " + str(r.returncode)]
        raise OSError(lines[-1].strip())
    return r.stdout


def build_inputs(checkout):
    """The bytes of every file under rust/ but target/, and of every file outside it the crate embeds,
    by checkout-relative path."""
    rust = checkout / "rust"
    files = {}
    for directory, dirs, names in os.walk(rust):
        if Path(directory) == rust and "target" in dirs:
            dirs.remove("target")
        for name in names:
            path = Path(directory, name)
            files[path.relative_to(checkout).as_posix()] = path.read_bytes()
    for rel in [rel for rel in files if rel.endswith(".rs")]:
        for embedded in EMBED.findall(files[rel].decode("utf-8", "replace")):
            path = Path(os.path.normpath(checkout / Path(rel).parent / embedded))
            files[path.relative_to(checkout).as_posix()] = path.read_bytes()
    return files


def private_dir(path):
    """path, created 0700, refused when it is a link, not a folder, another user's or open to others."""
    os.makedirs(path, mode=0o700, exist_ok=True)
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or os.name != "nt" and (info.st_uid != os.getuid() or info.st_mode & 0o077):
        raise OSError(str(path) + " is not a private folder")
    return path


def hold(lock):
    """Block until this process holds lock, an open file; closing it releases the lock."""
    # Each lock module exists on one platform only, so neither is an import statement.
    if os.name == "nt":
        msvcrt = importlib.import_module("msvcrt")
        deadline = time.monotonic() + LOCK_WAIT
        while True:
            try:
                msvcrt.locking(lock.fileno(), msvcrt.LK_LOCK, 1)
                return
            except OSError as e:
                # LK_LOCK gives up after 10 s while another install builds; anything else is a real error.
                if e.errno not in (errno.EDEADLOCK, errno.EACCES) or time.monotonic() >= deadline:
                    raise
    fcntl = importlib.import_module("fcntl")
    fcntl.flock(lock.fileno(), fcntl.LOCK_EX)


def write_atomic(path, data):
    """Write data beside path, flush it to disk, then rename it over path."""
    temp = path.with_name(path.name + "." + uuid.uuid4().hex + ".tmp")
    with open(temp, "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    os.replace(temp, path)


def sync(files, root):
    """Make root hold exactly files beside its lock, writing only files whose bytes differ so the
    unchanged ones keep the mtimes cargo's fingerprints recorded."""
    for rel, data in files.items():
        path = root / rel
        if not path.is_file() or path.read_bytes() != data:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
    for directory, _, names in os.walk(root):
        for name in names:
            path = Path(directory, name)
            rel = path.relative_to(root).as_posix()
            if rel != LOCK and rel not in files:
                path.unlink()


def linked(path):
    info = path.lstat()
    return stat.S_ISLNK(info.st_mode) or bool(getattr(info, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT)


def safe_parents(path):
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError()
    for parent in (path,) + tuple(path.parents):
        if os.path.lexists(parent) and (linked(parent) or not parent.is_dir()):
            raise ValueError()


def tree(root, source=False):
    if linked(root) or not root.is_dir():
        raise ValueError()
    files, identities, directories = {}, {}, set()
    def scan_error(error):
        raise error
    for directory, dirs, names in os.walk(root, onerror=scan_error):
        for name in dirs + names:
            path = Path(directory, name)
            info = path.lstat()
            if linked(path) or not os.access(path, os.R_OK):
                raise ValueError()
            rel = path.relative_to(root).as_posix()
            identities[rel] = (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)
            if name in dirs:
                directories.add(rel)
                if source and (name.startswith(".") or name == "__pycache__"):
                    dirs.remove(name)
                continue
            if not stat.S_ISREG(info.st_mode):
                raise ValueError()
            if source and (name.startswith(".") or path.suffix in (".pyc", ".pyo")):
                continue
            files[rel] = path.read_bytes()
    if source:
        expected_dirs = {str(Path(name).parent).replace(os.sep, "/") for name in REQUIRED} - {"."}
        filtered_dirs = {d for d in directories if not any(p.startswith(".") or p == "__pycache__" for p in d.split("/"))}
        if set(files) != set(REQUIRED) or filtered_dirs != expected_dirs:
            raise ValueError()
    return files, identities


def stamp(source, content):
    """bundle.json text naming the source's Git commit and a hash of the copied files, or None
    when the source is not in a Git checkout."""
    try:
        head = subprocess.run(["git", "-C", str(source), "rev-parse", "--verify", "HEAD"],
                              capture_output=True, text=True)
    except OSError:
        return None
    commit = head.stdout.strip()
    if head.returncode or not re.fullmatch(r"[0-9a-f]{40}", commit):
        return None
    tree = hashlib.sha256()
    for rel in sorted(content):
        tree.update(rel.encode() + b"\0" + hashlib.sha256(content[rel]).digest())
    return json.dumps({"commit": commit, "tree": tree.hexdigest()}, sort_keys=True) + "\n"


def drop_legacy(destination):
    path = destination / LEGACY
    if not os.path.lexists(path):
        return
    if not linked(path):
        print("install.py: " + str(path) + " is a folder, not a link; remove it by hand so agents load only factory",
              file=sys.stderr)
        return
    try:
        os.unlink(path)
    except OSError:
        os.rmdir(path)
    print("removed " + str(path))


def identity(path):
    info = path.lstat()
    return info.st_dev, info.st_ino, info.st_mtime_ns


def install(source: Path, destination: Path, replace: bool = False) -> Path:
    source, destination = Path(source), Path(destination)
    current = source
    stage = None
    lock_fd = None
    lock_identity = None
    backup = None
    try:
        safe_parents(source)
        safe_parents(destination)
        source, destination = source.resolve(), destination.resolve()
        if source == destination or source in destination.parents or destination in source.parents:
            raise ValueError()
        content, _ = tree(source, source=True)
        binary = rust_binary(source.parent.parent)
        if binary is not None:
            content[BINARY] = binary
        bundle = stamp(source, content)
        if bundle is not None and binary is not None:
            # catch_up_entry runs a skill-relative entry; an absolute one falls back to Python.
            bundle = json.dumps(dict(json.loads(bundle), entry=[BINARY],
                                     binary_sha256=hashlib.sha256(binary).hexdigest()), sort_keys=True) + "\n"
        if bundle is not None:
            content["bundle.json"] = bundle.encode()
        target = destination / "factory"
        current = target
        if os.path.lexists(target) and (linked(target) or not target.is_dir()):
            raise ValueError()
        destination.mkdir(parents=True, exist_ok=True)
        lock = destination / ".factory-install.lock"
        current = lock
        lock_fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        info = os.fstat(lock_fd)
        lock_identity = (info.st_dev, info.st_ino)
        os.close(lock_fd)
        lock_fd = None
        current = target
        prior_identity = identity(target) if os.path.lexists(target) else None
        prior = tree(target) if prior_identity is not None else None
        required_dirs = {str(Path(name).parent).replace(os.sep, "/") for name in REQUIRED} - {"."}
        if prior is not None and prior[0] == content and set(prior[1]) == set(content) | required_dirs:
            print(str(target))
            current = destination / LEGACY
            drop_legacy(destination)
            return target
        if prior is not None and not replace:
            fail(f"{target}: existing install differs; use --replace explicitly")
        stage = Path(tempfile.mkdtemp(prefix=".factory-stage-", dir=str(destination)))
        for name, data in content.items():
            path = stage / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            if name == BINARY:
                path.chmod(0o755)
        if prior_identity is not None:
            if not os.path.lexists(target) or identity(target) != prior_identity or tree(target) != prior:
                fail(f"{target}: destination changed during installation")
            backup = destination / ("factory.backup-" + uuid.uuid4().hex)
            if os.path.lexists(backup):
                fail(f"{backup}: backup already exists")
            os.rename(target, backup)
        try:
            if os.path.lexists(target):
                fail(f"{target}: competing destination appeared; retained prior backup")
            os.rename(stage, target)
            stage = None
        except BaseException:
            if backup is not None and not os.path.lexists(target):
                os.rename(backup, target)
            raise
        print(str(target))
        if backup is not None:
            try:
                shutil.rmtree(backup)
                print("removed " + str(backup))
            except OSError:
                print("install.py: could not remove " + str(backup) + "; remove it by hand", file=sys.stderr)
        current = destination / LEGACY
        drop_legacy(destination)
        return target
    except (OSError, ValueError):
        fail(f"{current}: cannot safely install factory")
    finally:
        try:
            try:
                if lock_fd is not None:
                    os.close(lock_fd)
                if stage is not None:
                    shutil.rmtree(stage)
            finally:
                if lock_identity is not None and os.path.lexists(lock):
                    info = lock.lstat()
                    if (info.st_dev, info.st_ino) == lock_identity:
                        lock.unlink()
        except OSError:
            fail(f"{destination}: cannot clean this installation attempt")


def main(argv=None) -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", required=True, type=Path)
    parser.add_argument("--replace", action="store_true")
    args = parser.parse_args(argv)
    source = Path(__file__).resolve().parents[1] / "skills/factory"
    install(source, args.destination, args.replace)


if __name__ == "__main__":
    main()
