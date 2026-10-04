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
