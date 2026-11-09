"""Copy the self-contained factory skill. Python 3.8+, stdlib only."""

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path


REQUIRED = (
    "SKILL.md", "runtime/factory.py", "runtime/paseo_stalls.py", "runtime/pr_screenshots.py",
    "templates/config.md", "templates/project.md", "references/execution.md", "references/integrations.md",
    "references/pickup.md", "references/visuals.md", "references/walkthrough.html", "schedules/explore.md", "schedules/resume.md",
) + tuple("agents/factory-" + name + ".md" for name in (
    "approver", "builder", "explorer", "review-code", "review-security", "review-ui", "review-ux", "spec-writer"))


def fail(message):
    print("install.py: " + message, file=sys.stderr)
    raise SystemExit(2)


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
        bundle = stamp(source, content)
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
            return target
        if prior is not None and not replace:
            fail(f"{target}: existing install differs; use --replace explicitly")
        stage = Path(tempfile.mkdtemp(prefix=".factory-stage-", dir=str(destination)))
        for name, data in content.items():
            path = stage / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
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
            print(str(backup))
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
