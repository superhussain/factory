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
  echo "install: cargo not found; install rustup from https://rustup.rs, then run the install again" >&2
  exit 2
fi
# cwd is the workspace so rustup reads its rust-toolchain.toml.
(cd "$checkout/rust" && "$cargo" build --profile host --locked)
target=${CARGO_TARGET_DIR:-$checkout/rust/target}
exec "$target/host/factory" install --source "$checkout" --destination "$1" --replace
