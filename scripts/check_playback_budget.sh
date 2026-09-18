#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
# Budgets are optional. Set an explicit case and hardware profile when gating.
# The benchmark refuses missing GPU/FFmpeg when any budget is configured.
cargo test -p photonic-video --release --test playback_throughput_bench -- \
  --ignored --nocapture --test-threads=1 "$@"
