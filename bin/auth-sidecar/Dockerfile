FROM rust:1-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY bin ./bin
RUN cargo build --release --package auth-sidecar

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --create-home --shell /usr/sbin/nologin tower
COPY --from=builder /build/target/release/auth-sidecar /usr/local/bin/auth-sidecar
USER tower
EXPOSE 8082
ENTRYPOINT ["/usr/local/bin/auth-sidecar"]
