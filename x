#!/usr/bin/env bash
# Run a command inside the project's devcontainer (Rust toolchain lives there,
# not on the host). Usage: ./x cargo build   |   ./x cargo test
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
exec devcontainer exec --workspace-folder "$here" "$@"
