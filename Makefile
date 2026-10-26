.PHONY: build test check package native-package clean

build:
	cargo build --release --locked --bin clipbridge

test:
	cargo test --locked
	cargo test --manifest-path remote/Cargo.toml --locked

check:
	cargo fmt --check
	cargo clippy --all-targets --locked -- -D warnings
	cargo fmt --manifest-path remote/Cargo.toml --check
	cargo clippy --manifest-path remote/Cargo.toml --all-targets --locked -- -D warnings

package:
	cargo run --release --locked --bin package

native-package: build
	./scripts/package-native.sh --target "$$(rustc -vV | sed -n 's/^host: //p')" --binary target/release/clipbridge

clean:
	cargo clean
