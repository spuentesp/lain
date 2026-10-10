# Lain — local MCP server for cross-repo and per-repo code analysis.
#
# `make schema` regenerates docs/tool-schema.json from the live
# `tools/list` payload (defect D-L2). CI runs this on every PR and
# fails the build if `git diff --exit-code docs/tool-schema.json`
# reports any change.

.PHONY: schema record-demo record-demo-small demo-video

schema:
	cargo run --quiet --bin lain -- schema dump --out docs/tool-schema.json

record-demo:
	./scripts/record-spa-demo.sh            # default: --fixture real (bytes + tokio)

record-demo-small:
	./scripts/record-spa-demo.sh --fixture synthetic   # offline, original fixture

# Generate the ~3-minute demo MP4 (docs/video/lain-demo.mp4).
# Run against synthetic fixture by default (fast, offline). Pass --fixture real
# for the hero shot with bytes + tokio (slow, requires GitHub access).
demo-video:
	./scripts/make-demo-video.sh

demo-video-real:
	./scripts/make-demo-video.sh --fixture real

# ---- Formal verification & deterministic checking (docs/formal/README.md) ----
# None of these run in CI yet; run them locally before touching concurrency code.
.PHONY: formal loom kani miri mutants proptest verify

# TLA+: every spec in docs/formal/MANIFEST must match its expected outcome.
formal:
	./scripts/check-formal.sh

# loom: exhaustive thread-interleaving tests on the real code. Own target dir
# (separate cfg) so it does not invalidate the normal build.
loom:
	RUSTFLAGS="--cfg lain_loom" CARGO_TARGET_DIR=target/loom cargo test --lib loom_

# Kani bounded model checking (needs `cargo install kani-verifier && cargo kani setup`).
kani:
	cargo kani --lib

# Miri on the unsafe-adjacent tests (needs nightly + miri component).
miri:
	MIRIFLAGS="-Zmiri-disable-isolation" CARGO_TARGET_DIR=target/miri \
	  cargo +nightly miri test --lib sensors::util::verification

# Mutation testing of the verified modules (slow; needs cargo-mutants). Each
# mutant runs only the tests of the module it touches, and the build is
# `--lib` without debuginfo: the default builds every integration-test binary
# and can fill the disk.
mutants:
	CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
	  cargo mutants -f src/server/readiness.rs -f src/server/reload.rs \
	  --cargo-arg=--lib --timeout 120 -- -- server::readiness server::reload

# proptest state machines and property tests.
proptest:
	cargo test --lib verification

verify: formal proptest loom
