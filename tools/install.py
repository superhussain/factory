"""Build this checkout's factory binary, then run its install. Python 3.8+, stdlib only.

A shim until tools/install.sh and tools/install.ps1 replace it: the build is the same cached host-profile
build `factory install` runs, so the binary's own install finds it in the cache and builds nothing.
"""

import argparse
import errno
import hashlib
import os
import re
import shutil
import stat
import subprocess
import sys
import time
import uuid
from pathlib import Path


BINARY = "runtime/factory.exe" if os.name == "nt" else "runtime/factory"
LOCK = ".build.lock"
PROFILE = "host"
KEEP = 3  # binaries kept in the cache, newest first
LOCK_WAIT = 1800  # seconds a Windows install waits for another one's build
# A literal include_str! or include_bytes! path, relative to the .rs file that holds it.
EMBED = re.compile(r'include_(?:str|bytes)!\(\s*"([^"]+)"\s*\)')
NO_CARGO = ("cargo not found; install rustup with rustup-init.exe from https://win.rustup.rs/x86_64 (see https://rustup.rs), "
            "then run the install again" if os.name == "nt" else
            "cargo not found; install rustup with curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh "
            "(see https://rustup.rs), then run the install again")


def fail(message):
    print("install: " + message, file=sys.stderr)
    raise SystemExit(2)


def rust_binary(checkout):
    """The path of the host-profile binary built from checkout's rust/ folder, from the cache when one
    was built from the same inputs. Exits 2 when there is no Rust workspace, no cargo or the build fails."""
    if not (checkout / "rust" / "Cargo.toml").is_file():
        fail(str(checkout) + " has no rust/Cargo.toml")
    cargo = shutil.which("cargo")
    if cargo is None:
        # rustup installs into ~/.cargo/bin, which launchd and non-login shells leave off PATH.
        home_cargo = Path.home() / ".cargo" / "bin" / ("cargo.exe" if os.name == "nt" else "cargo")
        if home_cargo.is_file() and os.access(home_cargo, os.X_OK):
            cargo = str(home_cargo)
    if cargo is None:
        fail(NO_CARGO)
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
                os.chmod(stored / name, 0o700)
                print("install: Rust binary from cache " + key[:8], file=sys.stderr)
                return stored / name
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
            os.chmod(stored / name, 0o700)
            write_atomic(stored / (name + ".sha256"), (hashlib.sha256(binary).hexdigest() + "\n").encode())
            os.utime(stored)
            others = [path for path in stored.parent.iterdir() if path != stored]
            for old in sorted(others, key=lambda path: path.stat().st_mtime_ns, reverse=True)[KEEP - 1:]:
                shutil.rmtree(old)
        print("install: Rust binary built in " + str(round(time.monotonic() - start)) + " s", file=sys.stderr)
        return stored / name
    except (OSError, ValueError) as e:
        fail("Rust build failed (" + str(e) + ")")


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
    if os.name == "nt":
        import msvcrt
        deadline = time.monotonic() + LOCK_WAIT
        while True:
            try:
                msvcrt.locking(lock.fileno(), msvcrt.LK_LOCK, 1)
                return
            except OSError as e:
                # LK_LOCK gives up after 10 s while another install builds; anything else is a real error.
                if e.errno not in (errno.EDEADLOCK, errno.EACCES) or time.monotonic() >= deadline:
                    raise
    import fcntl
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


def main(argv=None) -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", required=True, type=Path)
    parser.add_argument("--replace", action="store_true")
    args = parser.parse_args(argv)
    checkout = Path(__file__).resolve().parents[1]
    binary = rust_binary(checkout)
    command = [str(binary), "install", "--source", str(checkout), "--destination", str(args.destination)]
    sys.stderr.flush()
    raise SystemExit(subprocess.run(command + (["--replace"] if args.replace else [])).returncode)


if __name__ == "__main__":
    main()
