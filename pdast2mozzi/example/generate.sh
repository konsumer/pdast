#!/usr/bin/env bash
# Regenerate src/main.ino from patch.pd using pdast's CLI tools.
#
# Uses pd2ast/pdast2mozzi from PATH if installed (`cargo install --path
# ../../pd2ast` and `cargo install --path ..`), otherwise falls back to
# `cargo run` against this repo's workspace — so this works right out of a
# fresh clone with no install step.
set -euo pipefail
cd "$(dirname "$0")"

if command -v pd2ast >/dev/null 2>&1 && command -v pdast2mozzi >/dev/null 2>&1; then
  pd2ast patch.pd | pdast2mozzi - -o src/main.ino
else
  echo "pd2ast/pdast2mozzi not found on PATH — running via 'cargo run' instead" >&2
  cargo run -q -p pd2ast --manifest-path ../../Cargo.toml -- patch.pd \
    | cargo run -q -p pdast2mozzi --manifest-path ../../Cargo.toml -- - -o src/main.ino
fi

echo "wrote src/main.ino"
