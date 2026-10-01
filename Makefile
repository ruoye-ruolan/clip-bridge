.PHONY: build test check package clean

build:
	cargo build --release --locked --bin clipbridge

test:
	cargo test --locked

check:
	cargo fmt --check
	cargo clippy --all-targets --locked -- -D warnings

package:
	cargo run --release --locked --bin package

clean:
	cargo clean
