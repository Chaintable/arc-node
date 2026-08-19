#!/usr/bin/env bash

# Copyright 2026 Circle Internet Group, Inc. All rights reserved.
#
# SPDX-License-Identifier: Apache-2.0
#
# Runs the reusable unit and launched-node tests for trace_debankBlock output.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"

cargo test --locked -p debank-rpc --lib
cargo test --locked -p arc-execution-e2e \
  --test debank_eip7702 \
  --test debank_trace \
  --test debank_trace_edge_cases \
  --test debank_precompiles \
  --test debank_system_contracts \
  -- --test-threads=1
