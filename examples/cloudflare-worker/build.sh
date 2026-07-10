#!/usr/bin/env bash
# Builds knack-search-wasm and generates the JS glue this worker
# imports (./pkg/knack_search_wasm.js). Run manually, or let wrangler
# run it automatically via wrangler.toml's [build] block on every
# `wrangler dev` / `wrangler deploy`.
#
# Requires:
#   - the wasm32-unknown-unknown target: rustup target add wasm32-unknown-unknown
#   - wasm-bindgen-cli matching the wasm-bindgen version in
#     knack-search-wasm/Cargo.toml:
#       cargo install wasm-bindgen-cli --version <version>
#     A mismatched wasm-bindgen-cli version will fail loudly at
#     generation time rather than silently producing broken glue, so
#     if this script errors out, check that version first.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"

cargo build \
    --manifest-path "${repo_root}/knack-search-wasm/Cargo.toml" \
    --target wasm32-unknown-unknown \
    --release

wasm-bindgen \
    --target bundler \
    --out-dir "${script_dir}/pkg" \
    --out-name knack_search_wasm \
    "${repo_root}/target/wasm32-unknown-unknown/release/knack_search_wasm.wasm"

echo "generated ${script_dir}/pkg from knack-search-wasm"
