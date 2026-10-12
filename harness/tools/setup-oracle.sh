#!/usr/bin/env bash
# Set up the native-HiGHS oracle (independent of the highs-js wasm build).
# Creates harness/tools/oracle-venv with `highspy` (native HiGHS).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
venv="$here/oracle-venv"
python3 -m venv "$venv"
"$venv/bin/pip" install --quiet --upgrade pip
"$venv/bin/pip" install --quiet highspy
echo "oracle venv ready: $venv"
echo "run: $venv/bin/python $here/native-oracle.py harness/fixtures/planning-s.lp --time 20"
