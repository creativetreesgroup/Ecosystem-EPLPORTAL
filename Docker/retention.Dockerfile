FROM rust:1-slim-bookworm AS builder
WORKDIR /build
COPY Backend/Cargo.toml Backend/Cargo.lock ./
COPY Backend/crates ./crates
COPY Backend/bin ./bin
RUN cargo build --release --package retention

FROM debian:bookworm-slim AS runtime
RUN useradd --system --create-home --shell /usr/sbin/nologin tower
COPY --from=builder /build/target/release/retention /usr/local/bin/retention
USER tower
ENTRYPOINT ["/usr/local/bin/retention"]
