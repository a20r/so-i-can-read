# so-i-can-read

A Rapid Serial Visual Presentation reader for links, pasted text, markdown and Slack messages. Rust compiled to WebAssembly, deployed to GitHub Pages.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129
./build.sh                 # writes the static site to dist/
python3 -m http.server -d dist 8000
cargo test                 # unit tests for the text pipeline
```
