#!/usr/bin/env bash
# Start Cascades in fixture mode — no live API keys required.
# All data sources return embedded canned responses.
#
# Usage:
#   ./scripts/dev-server.sh
#
# The server listens on http://localhost:8080 by default.
# Override the port in config.toml: [server] port = 9090
set -euo pipefail

cd "$(dirname "$0")/.."

export RUST_LOG="${RUST_LOG:-info}"
export SKAGIT_FIXTURE_DATA=1

echo "Starting Cascades dev server (fixture mode) on http://localhost:8080"
echo "  GET http://localhost:8080/image.png"
echo ""

exec cargo run
