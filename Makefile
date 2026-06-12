.PHONY: fmt test clippy check install doctor production-acceptance release-dry-run

fmt:
	cargo fmt --check

test:
	cargo test

clippy:
	cargo clippy -- -D warnings

check: fmt test clippy

install:
	cargo install --path .

doctor:
	cargo run -- doctor --json

production-acceptance:
	./scripts/production-acceptance.sh

release-dry-run:
	rm -rf dist
	mkdir -p dist/package
	cargo build --release --target aarch64-apple-darwin
	cp target/aarch64-apple-darwin/release/simx dist/package/simx
	cp README.md dist/package/README.md
	if [ -f LICENSE ]; then cp LICENSE dist/package/LICENSE; fi
	if [ -f CHANGELOG.md ]; then cp CHANGELOG.md dist/package/CHANGELOG.md; fi
	tar -C dist/package -czf dist/simx-aarch64-apple-darwin.tar.gz .
	cd dist && shasum -a 256 simx-aarch64-apple-darwin.tar.gz > checksums.txt
