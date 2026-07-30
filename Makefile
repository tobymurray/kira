# Kira. `make help` lists the targets.

WASM_TARGET := wasm32-unknown-unknown
WASM_OUT    := site/lib
WASM_BIN    := target/$(WASM_TARGET)/wasm-release/kira_wasm.wasm

.PHONY: help check test wasm serve icons fmt lint clean

help:
	@echo "make check   fmt, clippy and tests"
	@echo "make test    run the test suite"
	@echo "make wasm    build the browser module into $(WASM_OUT)"
	@echo "make serve   build wasm, then serve site/ on :8099"
	@echo "make icons   regenerate favicons from assets/kira-mark.png"
	@echo ""
	@echo "The catalogue needs release binaries, so it is not a make target:"
	@echo "  cargo run -p kira-cli -- build --src <dir> --out site [--releases r.json]"

check: fmt lint test

fmt:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy -p kira-wasm --target $(WASM_TARGET) -- -D warnings

test:
	cargo test --workspace

# The browser half. wasm-bindgen must match the wasm-bindgen crate version;
# `cargo install wasm-bindgen-cli --locked` picks it up from Cargo.lock.
wasm:
	cargo build -p kira-wasm --target $(WASM_TARGET) --profile wasm-release
	wasm-bindgen $(WASM_BIN) --out-dir $(WASM_OUT) --target web --no-typescript

serve: wasm
	cargo run -p kira-cli -- serve

icons:
	cargo run -p kira-cli -- icons

clean:
	cargo clean
	rm -rf site/data site/lib
