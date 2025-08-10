# charms-lib

## Prerequisites

```sh
brew install llvm
export PATH="/opt/homebrew/opt/llvm/bin:$PATH"
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli
```

## Building

In this directory:

```sh
RUSTFLAGS="-C target-cpu=generic" cargo build --release --target wasm32-unknown-unknown

wasm-bindgen --out-dir target/wasm-bindgen-nodejs --target nodejs ../target/wasm32-unknown-unknown/release/charms_lib.wasm
```

## Testing

In this directory:

```sh
node test/extractAndVerifySpell.node.test.js
```
