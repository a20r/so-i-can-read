#!/usr/bin/env bash
# Builds the wasm bundle and assembles the static site in dist/.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release --target wasm32-unknown-unknown
rm -rf dist
mkdir -p dist
wasm-bindgen --target web --no-typescript --out-dir dist/pkg \
  target/wasm32-unknown-unknown/release/so_i_can_read.wasm
cp web/index.html web/style.css web/icon.svg web/manifest.webmanifest dist/
touch dist/.nojekyll
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Os -o dist/pkg/so_i_can_read_bg.wasm dist/pkg/so_i_can_read_bg.wasm
fi
ls -la dist dist/pkg
