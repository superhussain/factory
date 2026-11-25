#!/bin/sh
# Build this checkout's factory binary, then install the skill with it. POSIX sh, no Python.
# Usage: tools/install.sh <absolute skills root>, for example tools/install.sh "$HOME/.agents/skills"
set -eu
if [ "$#" -ne 1 ]; then
  echo "usage: tools/install.sh <absolute skills root>" >&2
  exit 2
fi
checkout=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
cargo=$(command -v cargo || true)
# rustup installs into ~/.cargo/bin, which non-login shells can leave off PATH.
if [ -z "$cargo" ] && [ -x "$HOME/.cargo/bin/cargo" ]; then
  cargo=$HOME/.cargo/bin/cargo
fi
if [ -z "$cargo" ]; then
  echo "install: cargo not found; install rustup with curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh (see https://rustup.rs), then run the install again" >&2
  exit 2
fi
# The stamp factory install checks before it trusts this build: a hash of every file under rust/ but
# rust/target and under skills/factory, then the binary's sha256. The inputs are hashed before the build, so
# an edit made during it does not match. A changed input or binary means a rebuild.
if command -v sha256sum >/dev/null 2>&1; then sum=sha256sum; else sum="shasum -a 256"; fi
inputs=$(cd "$checkout" && find rust skills/factory -path rust/target -prune -o -type f -print0 | LC_ALL=C sort -z | xargs -0 $sum | $sum | cut -c1-64)
# cwd is the workspace so rustup reads its rust-toolchain.toml. The build stays in rust/target, where
# factory install recognizes its own fresh build and caches it instead of compiling the crate again.
(unset CARGO_TARGET_DIR && cd "$checkout/rust" && "$cargo" build --profile host --locked)
(
  cd "$checkout"
  umask 077
  binary=$($sum < rust/target/host/factory | cut -c1-64)
  rm -f rust/target/host/factory.stamp
  printf '%s\n%s\n' "$inputs" "$binary" > rust/target/host/factory.stamp
)
exec "$checkout/rust/target/host/factory" install --source "$checkout" --destination "$1" --replace
