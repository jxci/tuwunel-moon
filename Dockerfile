# Production / fork: multi-stage build (no BuildKit cache mounts — works on plain docker build).
# Binary: workspace package `tuwunel`. Runtime matches Dockerfile.local (bookworm-slim + liburing).
FROM rust:1.94-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
	build-essential cmake ninja-build clang llvm-dev libclang-dev \
	libssl-dev pkg-config liburing-dev \
	&& rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
RUN cargo build --release --locked -p tuwunel \
	&& install -m755 target/release/tuwunel /tmp/tuwunel

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
	ca-certificates libssl3 liburing2 \
	&& rm -rf /var/lib/apt/lists/*

COPY --from=builder /tmp/tuwunel /usr/local/bin/tuwunel

# Matrix client/federation; prod Traefik targets 6167 in your compose.
EXPOSE 6167 8008 8448
ENTRYPOINT ["/usr/local/bin/tuwunel"]
