#!/bin/sh
# Builds the browser version into out/web/. Serve that folder with any static server, e.g.
#   python3 -m http.server -d out/web 8000   →   http://localhost:8000
# Needs the wasm target: rustup target add wasm32-unknown-unknown
set -e
cd "$(dirname "$0")/.."
CARGO=cargo
if command -v rustup >/dev/null 2>&1; then
    # a Homebrew/distro Rust on PATH has no wasm target: use rustup's toolchain explicitly
    export RUSTC="$(rustup which rustc)"
    CARGO="$(rustup which cargo)"
fi
"$CARGO" build --release --target wasm32-unknown-unknown --bin reformers
rm -rf out/web
mkdir -p out/web
cp web/index.html web/mq_js_bundle.js target/wasm32-unknown-unknown/release/reformers.wasm out/web/
cp -R assets out/web/
echo "out/web ready"
