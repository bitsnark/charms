# charms-lib

## Prerequisites

Install LLVM, Rust Wasm target support and wasm-bindgen CLI:

```sh
brew install llvm
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
```

Make sure LLVM is in your path:

```sh
export PATH="/opt/homebrew/opt/llvm/bin:$PATH"
```

## Building

In this directory:

```sh
RUSTFLAGS="-C target-cpu=generic" wasm-pack build --target nodejs
```

## Testing

In this directory:

```sh
node test/extractAndVerifySpell.node.test.js
```
