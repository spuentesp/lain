# Lain — local MCP server for cross-repo and per-repo code analysis.
#
# `make schema` regenerates docs/tool-schema.json from the live
# `tools/list` payload (defect D-L2). CI runs this on every PR and
# fails the build if `git diff --exit-code docs/tool-schema.json`
# reports any change.

.PHONY: schema

schema:
	cargo run --quiet -- schema dump --out docs/tool-schema.json
