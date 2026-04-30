# Stage 1: Build
FROM rust:1.88-bookworm AS builder

# Install build dependencies
# We need cmake for tree-sitter and other C-based dependencies
# pkg-config and libssl-dev for git2/openssl
# clang for bindgen (often used by C-wrapper crates)
RUN apt-get update && apt-get install -y \
    cmake \
    pkg-config \
    libssl-dev \
    git \
    clang \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/lain

# Copy Cargo files
COPY Cargo.toml Cargo.lock ./

# Create dummy source to cache dependencies
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs && \
    echo "" > src/lib.rs

# Build dependencies only
RUN cargo build --release

# Remove dummy source
RUN rm -rf src

# Copy real source
COPY . .

# Build real binary
# Ensure main.rs and lib.rs are newer than the dummy ones
RUN touch src/main.rs src/lib.rs
RUN cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim

# Install runtime dependencies
# git: required for co-change analysis
# ca-certificates: required for downloading models/LSP servers
# libssl3: required for git2/openssl
# libgomp1: required by ONNX Runtime for OpenMP support
RUN apt-get update && apt-get install -y \
    git \
    ca-certificates \
    libssl3 \
    libgomp1 \
    && rm -rf /var/lib/apt/lists/*

# Create workspace directory
WORKDIR /workspace

# Copy binary from builder
COPY --from=builder /usr/src/lain/target/release/lain /usr/local/bin/lain

# Note: ort usually downloads onnxruntime to target/release/build/ort-*/out/
# If dynamically linked, we might need to find and copy it.
# However, many setups use static linking or standard system paths.
# If you encounter "library not found" errors, uncomment the following line
# (you may need to adjust the path based on the specific ort build output):
# COPY --from=builder /usr/src/lain/target/release/build/ort-*/out/libonnxruntime.so* /usr/lib/

# Set entrypoint
ENTRYPOINT ["/usr/local/bin/lain"]

# Default command - runs in stdio mode for MCP clients like Claude Code
CMD ["--workspace", "/workspace", "--transport", "stdio"]
