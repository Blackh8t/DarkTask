#!/usr/bin/env bash
set -euo pipefail
# Developer/bootstrap convenience only. Production releases should deploy the CI-built binary.
command -v cargo >/dev/null || { echo "cargo is required for this bootstrap build" >&2; exit 1; }
cargo build --release -p remote-server
exec sudo ./infra/systemd/install-server.sh ./target/release/remote-server
