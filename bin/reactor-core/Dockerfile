FROM rust:1-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY bin ./bin
RUN cargo build --release --package reactor-core

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --create-home --shell /usr/sbin/nologin tower
COPY --from=builder /build/target/release/reactor-core /usr/local/bin/reactor-core
USER tower
EXPOSE 8081
ENTRYPOINT ["/usr/local/bin/reactor-core"]
