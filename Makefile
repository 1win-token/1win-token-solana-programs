export PATH := $(HOME)/.local/share/solana/install/active_release/bin:$(PATH)
export NO_DNA := 1

.DEFAULT_GOAL := help
.PHONY: help tools math build build-sale build-burn build-vesting idl test test-sale test-vesting e2e e2e-vesting check fmt fmt-check lint id-check id-check-keys clean

PROGRAMS := sale burn vesting

help:
	@echo "tools           - anchor, solana and cargo versions"
	@echo "math            - unit tests of the program crates (no .so build)"
	@echo "build           - build every program"
	@echo "build-sale      - build Sale"
	@echo "build-burn      - build Burn"
	@echo "build-vesting   - build Vesting"
	@echo "idl             - generate the Sale and Vesting IDLs into target/idl"
	@echo "test            - build and test the whole workspace"
	@echo "test-sale       - build and test Sale and Burn (shared environment)"
	@echo "test-vesting    - build and test Vesting"
	@echo "e2e             - Sale e2e against surfpool (#[ignore], ports 8905-8910)"
	@echo "e2e-vesting     - Vesting e2e against surfpool (#[ignore], ports 8911-8915)"
	@echo "check           - fmt-check, lint, id-check, build, test"
	@echo "fmt             - format"
	@echo "fmt-check       - check formatting"
	@echo "lint            - clippy with -D warnings"
	@echo "id-check        - check declare_id! against Anchor.toml"
	@echo "id-check-keys   - informational check of declare_id! against the keypairs in target/deploy"
	@echo "clean           - cargo clean"

tools:
	@anchor --version && solana --version && cargo --version

math:
	cargo test -p sale -p burn -p vesting --locked

build:
	anchor build --no-idl --ignore-keys -- -- --locked

# Builds a single program: if one fails to build, the other's tests can still be
# run.
build-sale:
	anchor build -p sale --no-idl --ignore-keys -- -- --locked

build-burn:
	anchor build -p burn --no-idl --ignore-keys -- -- --locked

build-vesting:
	anchor build -p vesting --no-idl --ignore-keys -- -- --locked

idl: build
	anchor idl build --program-name sale -o target/idl/sale.json
	anchor idl build --program-name vesting -o target/idl/vesting.json

test: build
	cargo test --workspace --locked

# Separate test targets: if Sale fails, the Vesting tests still run to completion,
# and vice versa.
#
# Sale and Burn share one environment (sale-test-common brings both programs up in
# LiteSVM), so the target builds both artifacts and runs both suites: building only
# sale.so is not an option - the freshness gate would not find burn.so and would
# fail.
test-sale: build-sale build-burn
	cargo test -p sale -p burn -p sale-test-common -p burn-tests --locked

test-vesting: build-vesting
	cargo test -p vesting -p vesting-test-common --locked

# The tests against surfpool are marked #[ignore] - an ordinary `make test` does not
# run them. Ports 8905-8910.
e2e: idl
	cargo test -p sale-e2e --locked -- --ignored --nocapture --test-threads=1

# The same for Vesting, on different ports so as not to interfere with the Sale e2e.
e2e-vesting: idl
	cargo test -p vesting-e2e --locked -- --ignored --nocapture --test-threads=1

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets --locked -- -D warnings

# Checks the program id from the code (declare_id!) against the address in
# Anchor.toml.
id-check:
	@ok=1; for p in $(PROGRAMS); do \
		src=$$(grep -o 'declare_id!("[^"]*")' programs/$$p/src/lib.rs | head -1 | sed 's/.*("//; s/")$$//'); \
		toml=$$(grep -E "^$$p = " Anchor.toml | head -1 | sed 's/.*"\(.*\)".*/\1/'); \
		if [ -n "$$src" ] && [ "$$src" = "$$toml" ]; then echo "id-check $$p: $$src"; \
		else echo "id-check $$p: MISMATCH declare_id!=$$src Anchor.toml=$$toml"; ok=0; fi; \
	done; [ $$ok = 1 ]

# An informational check: does declare_id! match the local keypair file? It affects
# nothing and never fails the build.
id-check-keys:
	@for p in $(PROGRAMS); do \
		src=$$(grep -o 'declare_id!("[^"]*")' programs/$$p/src/lib.rs | head -1 | sed 's/.*("//; s/")$$//'); \
		kp=target/deploy/$$p-keypair.json; \
		if [ -f $$kp ]; then key=$$(solana-keygen pubkey $$kp); else key='(keypair missing)'; fi; \
		if [ "$$src" = "$$key" ]; then echo "id-check-keys $$p: match ($$src)"; \
		else echo "id-check-keys $$p: declare_id!=$$src keypair=$$key"; fi; \
	done

check: fmt-check lint id-check build test

clean:
	cargo clean
