FROM rust:1-slim-bookworm AS builder
WORKDIR /build
COPY Backend/Cargo.toml Backend/Cargo.lock ./
COPY Backend/crates ./crates
COPY Backend/bin ./bin
RUN cargo build --release --package retention

FROM debian:bookworm-slim AS runtime
RUN useradd --system --create-home --shell /usr/sbin/nologin tower
COPY --from=builder /build/target/release/retention /usr/local/bin/retention
# Create + own the archive mountpoint BEFORE `USER tower`. A fresh named volume
# mounted here inherits the image directory's ownership, so this is what lets the
# non-root worker write `*.csv.gz` (File::create in stream_archive runs even under
# the compose default DRY_RUN=true). Without it the volume mounts root:root and the
# container's core function fails with EACCES.
RUN mkdir -p /archive && chown tower:tower /archive
USER tower
ENTRYPOINT ["/usr/local/bin/retention"]
