# --- Build Stage ---
FROM rust:1.81-slim-bookworm as builder

# Install build dependencies
RUN apt-get update && apt-get install -y pkg-config libssl-dev cmake g++ && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY . .

# Build the release binary
RUN cargo build --release

# --- Runtime Stage ---
# Distroless is tiny (~20MB) and very fast to start
FROM gcr.io/distroless/cc-debian12

WORKDIR /app

# Copy the binary and the models
COPY --from=builder /app/target/release/lain /usr/local/bin/lain
COPY --from=builder /app/models /app/models

# Default environment for models
ENV LAIN_MODELS_PATH=/app/models

# MCP Registry Ownership Verification
LABEL io.modelcontextprotocol.server.name="io.github.spuentesp/lain"

# Entrypoint
ENTRYPOINT ["/usr/local/bin/lain"]
CMD ["--workspace", "/workspace"]
