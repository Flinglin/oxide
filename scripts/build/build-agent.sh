#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
AGENT_MANIFEST="$ROOT_DIR/agent/Cargo.toml"
OUT_DIR="$ROOT_DIR/dist/agents"

mkdir -p "$OUT_DIR"
ARCH="$(uname -m)"

case "$ARCH" in
            x86_64|amd64)
                TARGETS=("x86_64-unknown-linux-gnu:x86_64-linux-gnu")
                ;;
            aarch64|arm64)
                TARGETS=("aarch64-unknown-linux-gnu:aarch64-linux-gnu")
                ;;
            *)
                echo "Unsupported Linux architecture: $ARCH"
                exit 1
                ;;
        esac
for pair in "${TARGETS[@]}"; do
  rust_target="${pair%%:*}"
  artifact_suffix="${pair##*:}"
  echo "==> Building oxide-agent for $rust_target"
  cargo build --manifest-path "$AGENT_MANIFEST" --release --target "$rust_target"
  cp "$ROOT_DIR/agent/target/$rust_target/release/oxide-agent" \
    "$OUT_DIR/oxide-agent-$artifact_suffix"
  chmod +x "$OUT_DIR/oxide-agent-$artifact_suffix"
  if command -v llvm-strip >/dev/null 2>&1; then
    # llvm-strip handles both bundled Linux agent targets from one host runner.
    llvm-strip "$OUT_DIR/oxide-agent-$artifact_suffix"
  fi
done

echo "Agent artifacts written to $OUT_DIR"
