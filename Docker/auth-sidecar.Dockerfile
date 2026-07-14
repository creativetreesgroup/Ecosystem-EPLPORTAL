FROM rust:1-slim-bookworm AS builder
WORKDIR /build
# cmake: build-time requirement of btls-sys (vendored BoringSSL fork pulled in
# transitively by spx-client's wreq dependency, Fase 3 Task 8). auth-sidecar
# does not depend on spx-client yet, so this is not load-bearing today, but
# once Fase 5's 3-tier auto-login wires spx-client into this binary's build
# graph, a missing cmake here would break the build silently until then —
# installed proactively so that day is a no-op instead of a debugging session.
RUN apt-get update && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*
COPY Backend/Cargo.toml Backend/Cargo.lock ./
COPY Backend/crates ./crates
COPY Backend/bin ./bin
RUN cargo build --release --package auth-sidecar

FROM debian:bookworm-slim AS runtime
# ca-certificates: spx-client's wreq dependency uses the OS-native trust store
# (webpki-roots' bundled Mozilla roots are disabled — CDLA-Permissive-2.0 is
# not in Backend/deny.toml's license allow-list, see Fase 3 Task 8) — any
# outbound HTTPS call needs this installed once spx-client is actually wired
# into auth-sidecar, or TLS verification fails closed with no trust anchors.
# chromium + fonts-liberation: chromiumoxide drives an EXTERNAL browser (it is
# NOT bundled). auth-sidecar's tier-1 SPX browser-login (Fase 5 Task 9) needs a
# real Chromium binary on PATH; the fonts avoid blank-render issues on some
# SSO pages. This makes the sidecar image intentionally heavier (~300MB more)
# than reactor-core's — expected, not a regression, given what this image does.
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates chromium fonts-liberation \
    && rm -rf /var/lib/apt/lists/*
ENV CHROME_BIN=/usr/bin/chromium
RUN useradd --system --create-home --shell /usr/sbin/nologin tower
COPY --from=builder /build/target/release/auth-sidecar /usr/local/bin/auth-sidecar
USER tower
EXPOSE 8082
ENTRYPOINT ["/usr/local/bin/auth-sidecar"]
