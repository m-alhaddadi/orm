#!/usr/bin/env bash
# Runs every benchmark (Python, Node, Bun, Go) over both transports, one at a time.
set -euo pipefail
cd "$(dirname "$0")"
PY=../.venv/bin/python
for t in unix tcp; do
  $PY run_bench.py --transport "$t"
  (cd js && node bench.mjs --transport "$t")
  (cd js && bun bench.mjs --transport "$t")
  (cd go && go run . -transport "$t")
done
$PY bridge_overhead.py > log-bridge.txt
