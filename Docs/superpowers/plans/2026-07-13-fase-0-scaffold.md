# TOWER Fase 0 — Scaffold & Fondasi Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the TOWER monorepo skeleton — empty Cargo workspace matching the target crate layout, a SvelteKit 5 placeholder app, and a Docker Compose topology that actually boots end-to-end — with zero business logic, so Fase 1 has real ground to build on.

**Architecture:** One Cargo workspace at the repo root with 8 empty lib crates (`core-domain`, `spx-client`, `poller`, `executor`, `store`, `ws-hub`, `notifier`, `api-gateway`) and 2 binaries (`reactor-core`, `auth-sidecar`), each binary exposing only a `/healthz` endpoint for now. A SvelteKit 5 + Tailwind v4 app (`web/`) renders one placeholder page that fetches `reactor-core`'s health through the edge proxy. Docker Compose wires all of this together locally behind Caddy, with Postgres 16 and Redis 7 present but unused until Fase 2+.

**Tech Stack:** Rust (stable, workspace resolver "2", edition 2021), tokio, axum; SvelteKit 5 (runes) + Tailwind v4 (`@theme`) + `adapter-node`; Docker Compose; Caddy 2; Postgres 16; Redis 7; GitHub Actions CI.

## Global Constraints

Full context: [`docs/tower-master-spec.md`](../../tower-master-spec.md) and [`docs/superpowers/specs/2026-07-13-fase-0-scaffold-design.md`](../specs/2026-07-13-fase-0-scaffold-design.md).

- No published Docker ports except the edge (Caddy), which binds `127.0.0.1` only. (Aturan Keras #8)
- Every container gets an explicit, unique name (`tower-*`) — never a generic alias like `api`. (Fase 0, Aturan Keras #8)
- All services share one dedicated Docker network (`tower-net`), never the default/shared network. (Aturan Keras #8)
- No plaintext secrets anywhere committed — `.env` is gitignored, only `.env.example` (placeholders) is tracked. (Aturan Keras #5)
- Fase 0 has **no business logic**. Any temptation to add real matching/DB/auth code belongs in Fase 1+.
- CI must run: `cargo build`, `cargo test`, `cargo clippy -D warnings`, `cargo sqlx prepare --check`, `gitleaks`, `cargo audit`, `cargo deny`. (Fase 0)
- Cargo workspace: `resolver = "2"`, `edition = "2021"`, `publish = false`. `Cargo.lock` is committed (workspace ships binaries).
- Rust/Cargo is not installed on this machine yet — Task 1 installs it before anything else can build.

---

### Task 1: Install Rust toolchain and supporting CLI tools

**Files:** None (machine-level tooling only, nothing in the repo changes).

**Interfaces:**
- Consumes: nothing.
- Produces: `cargo`, `rustc`, `rustup`, `cargo-audit`, `cargo-deny`, `sqlx` (cli), `gitleaks` on `PATH` — every later task assumes these exist.

- [x] **Step 1: Install rustup non-interactively (stable toolchain)**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile default
source "$HOME/.cargo/env"
```

- [x] **Step 2: Verify rustc/cargo are on PATH**

Run: `rustc --version && cargo --version`
Expected: two version lines, e.g. `rustc 1.8x.x (...)` and `cargo 1.8x.x (...)`. No "command not found".

- [x] **Step 3: Add clippy and rustfmt components**

```bash
rustup component add clippy rustfmt
```

- [x] **Step 4: Install cargo-audit and cargo-deny**

```bash
cargo install --locked cargo-audit cargo-deny
```

Expected: both binaries build and end with `Installed package ... (executable "cargo-audit")` / `"cargo-deny"`. This step is network- and CPU-bound; it can take several minutes.

- [x] **Step 5: Install sqlx-cli (Postgres + rustls only, to keep build time down)**

```bash
cargo install --locked sqlx-cli --no-default-features --features rustls,postgres
```

- [x] **Step 6: Install gitleaks via Homebrew**

```bash
brew install gitleaks
```

- [x] **Step 7: Verify every tool is callable**

Run: `cargo audit --version && cargo deny --version && sqlx --version && gitleaks version`
Expected: four version lines, no errors.

No commit for this task — it only installs local tooling, no repository files change.

---

### Task 2: Cargo workspace root and 8 empty lib crates

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `crates/core-domain/Cargo.toml`, `crates/core-domain/src/lib.rs`
- Create: `crates/spx-client/Cargo.toml`, `crates/spx-client/src/lib.rs`
- Create: `crates/poller/Cargo.toml`, `crates/poller/src/lib.rs`
- Create: `crates/executor/Cargo.toml`, `crates/executor/src/lib.rs`
- Create: `crates/store/Cargo.toml`, `crates/store/src/lib.rs`
- Create: `crates/ws-hub/Cargo.toml`, `crates/ws-hub/src/lib.rs`
- Create: `crates/notifier/Cargo.toml`, `crates/notifier/src/lib.rs`
- Create: `crates/api-gateway/Cargo.toml`, `crates/api-gateway/src/lib.rs`

**Interfaces:**
- Consumes: `cargo` from Task 1.
- Produces: workspace root `Cargo.toml` with a `members` list and a `[workspace.package]` block (`version.workspace = true`, `edition.workspace = true`, `publish.workspace = true`) that Task 3 and Task 4 will append `bin/reactor-core` and `bin/auth-sidecar` to.

- [x] **Step 1: Write the workspace root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = [
    "crates/core-domain",
    "crates/spx-client",
    "crates/poller",
    "crates/executor",
    "crates/store",
    "crates/ws-hub",
    "crates/notifier",
    "crates/api-gateway",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
publish = false

[profile.release]
lto = true
codegen-units = 1
```

- [x] **Step 2: Scaffold all 8 lib crates in one pass**

```bash
mkdir -p crates/{core-domain,spx-client,poller,executor,store,ws-hub,notifier,api-gateway}/src

for crate in core-domain spx-client poller executor store ws-hub notifier api-gateway; do
  cat > "crates/${crate}/Cargo.toml" <<EOF
[package]
name = "${crate}"
version.workspace = true
edition.workspace = true
publish.workspace = true
EOF
  touch "crates/${crate}/src/lib.rs"
done
```

- [x] **Step 3: Build the workspace**

Run: `cargo build --workspace`
Expected: `Compiling core-domain v0.1.0 (...)` through all 8 crates, ending `Finished \`dev\` profile [...] target(s) in ...s`. No errors.

- [x] **Step 4: Run the (empty) test suite**

Run: `cargo test --workspace`
Expected: `running 0 tests ... test result: ok. 0 passed; 0 failed` repeated once per crate. No failures.

- [x] **Step 5: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: `Finished` with no warnings/errors.

- [x] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/
git commit -m "feat: scaffold Cargo workspace with 8 empty lib crates"
```

---

### Task 3: `reactor-core` binary with health endpoint

**Files:**
- Modify: `Cargo.toml:5-13` (add `"bin/reactor-core"` to `members`)
- Create: `bin/reactor-core/Cargo.toml`
- Create: `bin/reactor-core/src/main.rs`
- Create: `bin/reactor-core/Dockerfile`

**Interfaces:**
- Consumes: workspace root `Cargo.toml` from Task 2.
- Produces: `fn app() -> axum::Router` in `bin/reactor-core/src/main.rs` (private to that binary, but Task 6/8 rely on the resulting service listening on `0.0.0.0:8081` and answering `GET /healthz` with `{"status":"ok","service":"reactor-core"}`). Docker image buildable from repo root via `bin/reactor-core/Dockerfile`.

- [x] **Step 1: Register the binary in the workspace**

Edit `Cargo.toml`, change the `members` array to:

```toml
members = [
    "crates/core-domain",
    "crates/spx-client",
    "crates/poller",
    "crates/executor",
    "crates/store",
    "crates/ws-hub",
    "crates/notifier",
    "crates/api-gateway",
    "bin/reactor-core",
]
```

- [x] **Step 2: Create the package and add dependencies**

```bash
mkdir -p bin/reactor-core/src
cat > bin/reactor-core/Cargo.toml <<'EOF'
[package]
name = "reactor-core"
version.workspace = true
edition.workspace = true
publish.workspace = true

[[bin]]
name = "reactor-core"
path = "src/main.rs"
EOF

cargo add --package reactor-core tokio --features rt-multi-thread,macros,signal,net
cargo add --package reactor-core axum
cargo add --package reactor-core tracing
cargo add --package reactor-core tracing-subscriber --features env-filter
cargo add --package reactor-core serde --features derive
cargo add --package reactor-core serde_json
cargo add --package reactor-core --dev tower --features util
cargo add --package reactor-core --dev http-body-util
```

- [x] **Step 3: Write the failing test first**

Create `bin/reactor-core/src/main.rs` with only the test module (the `app`/`healthz` symbols it references do not exist yet — this must fail to compile):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok_status() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["service"], "reactor-core");
    }
}
```

- [x] **Step 4: Run the test to verify it fails**

Run: `cargo test -p reactor-core`
Expected: FAIL — compile error, `cannot find function \`app\` in this scope` (and no `main` function either, since the crate has no binary entry point yet).

- [x] **Step 5: Write the full implementation**

Replace `bin/reactor-core/src/main.rs` entirely with:

```rust
use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

fn app() -> Router {
    Router::new().route("/healthz", get(healthz))
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "reactor-core" }))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("reactor-core starting");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8081")
        .await
        .expect("bind 0.0.0.0:8081");

    axum::serve(listener, app())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install ctrl_c handler");
    tracing::info!("reactor-core shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok_status() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["service"], "reactor-core");
    }
}
```

- [x] **Step 6: Run the test to verify it passes**

Run: `cargo test -p reactor-core`
Expected: `test tests::healthz_returns_ok_status ... ok`, `test result: ok. 1 passed; 0 failed`.

- [x] **Step 7: Clippy check**

Run: `cargo clippy -p reactor-core -- -D warnings`
Expected: clean, no warnings.

- [x] **Step 8: Write the Dockerfile (build context = repo root, not `bin/reactor-core/`)**

```dockerfile
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
```

Save as `bin/reactor-core/Dockerfile`.

- [x] **Step 9: Build and smoke-test the image standalone**

```bash
docker build -f bin/reactor-core/Dockerfile -t tower-reactor-core:dev .
docker run --rm -d --name reactor-core-smoke -p 18081:8081 tower-reactor-core:dev
sleep 2
curl -sf http://127.0.0.1:18081/healthz
docker stop reactor-core-smoke
```

Expected: `curl` prints `{"service":"reactor-core","status":"ok"}` (key order may vary), then the container stops cleanly.

- [x] **Step 10: Commit**

```bash
git add Cargo.toml Cargo.lock bin/reactor-core/
git commit -m "feat: add reactor-core binary with /healthz endpoint"
```

---

### Task 4: `auth-sidecar` binary with health endpoint

**Files:**
- Modify: `Cargo.toml:5-14` (add `"bin/auth-sidecar"` to `members`)
- Create: `bin/auth-sidecar/Cargo.toml`
- Create: `bin/auth-sidecar/src/main.rs`
- Create: `bin/auth-sidecar/Dockerfile`

**Interfaces:**
- Consumes: workspace root `Cargo.toml` from Task 3.
- Produces: same pattern as Task 3, but service name `"auth-sidecar"`, port `8082`.

- [x] **Step 1: Register the binary in the workspace**

Edit `Cargo.toml`, change `members` to append `"bin/auth-sidecar"` after `"bin/reactor-core"`:

```toml
members = [
    "crates/core-domain",
    "crates/spx-client",
    "crates/poller",
    "crates/executor",
    "crates/store",
    "crates/ws-hub",
    "crates/notifier",
    "crates/api-gateway",
    "bin/reactor-core",
    "bin/auth-sidecar",
]
```

- [x] **Step 2: Create the package and add dependencies**

```bash
mkdir -p bin/auth-sidecar/src
cat > bin/auth-sidecar/Cargo.toml <<'EOF'
[package]
name = "auth-sidecar"
version.workspace = true
edition.workspace = true
publish.workspace = true

[[bin]]
name = "auth-sidecar"
path = "src/main.rs"
EOF

cargo add --package auth-sidecar tokio --features rt-multi-thread,macros,signal,net
cargo add --package auth-sidecar axum
cargo add --package auth-sidecar tracing
cargo add --package auth-sidecar tracing-subscriber --features env-filter
cargo add --package auth-sidecar serde --features derive
cargo add --package auth-sidecar serde_json
cargo add --package auth-sidecar --dev tower --features util
cargo add --package auth-sidecar --dev http-body-util
```

- [x] **Step 3: Write the failing test first**

Create `bin/auth-sidecar/src/main.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok_status() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["service"], "auth-sidecar");
    }
}
```

- [x] **Step 4: Run the test to verify it fails**

Run: `cargo test -p auth-sidecar`
Expected: FAIL — compile error, `cannot find function \`app\` in this scope`.

- [x] **Step 5: Write the full implementation**

Replace `bin/auth-sidecar/src/main.rs` entirely with:

```rust
use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

fn app() -> Router {
    Router::new().route("/healthz", get(healthz))
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "auth-sidecar" }))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("auth-sidecar starting");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8082")
        .await
        .expect("bind 0.0.0.0:8082");

    axum::serve(listener, app())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install ctrl_c handler");
    tracing::info!("auth-sidecar shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok_status() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["service"], "auth-sidecar");
    }
}
```

- [x] **Step 6: Run the test to verify it passes**

Run: `cargo test -p auth-sidecar`
Expected: `test tests::healthz_returns_ok_status ... ok`, `test result: ok. 1 passed; 0 failed`.

- [x] **Step 7: Clippy check**

Run: `cargo clippy -p auth-sidecar -- -D warnings`
Expected: clean, no warnings.

- [x] **Step 8: Write the Dockerfile**

```dockerfile
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
```

Save as `bin/auth-sidecar/Dockerfile`.

- [x] **Step 9: Build and smoke-test the image standalone**

```bash
docker build -f bin/auth-sidecar/Dockerfile -t tower-auth-sidecar:dev .
docker run --rm -d --name auth-sidecar-smoke -p 18082:8082 tower-auth-sidecar:dev
sleep 2
curl -sf http://127.0.0.1:18082/healthz
docker stop auth-sidecar-smoke
```

Expected: `curl` prints `{"service":"auth-sidecar","status":"ok"}`, container stops cleanly.

- [x] **Step 10: Commit**

```bash
git add Cargo.toml Cargo.lock bin/auth-sidecar/
git commit -m "feat: add auth-sidecar binary with /healthz endpoint"
```

---

### Task 5: `web/` — SvelteKit 5 + Tailwind v4 placeholder app

**Files:**
- Create: `web/` (full SvelteKit 5 project scaffolded by the `sv` CLI)
- Modify: `web/svelte.config.js` (swap to `adapter-node`)
- Modify: `web/vite.config.ts` (add Tailwind v4 Vite plugin)
- Create: `web/src/app.css`
- Modify: `web/src/routes/+layout.svelte` (import `app.css`)
- Modify: `web/src/routes/+page.svelte` (placeholder content)
- Create: `web/Dockerfile`

**Interfaces:**
- Consumes: nothing from earlier tasks (independent stack).
- Produces: a Node server started by `node build` listening on `$PORT` (default `3000`), serving `GET /` and depending on a same-origin `/api/healthz` (proxied by Caddy in Task 6) to display `reactor-core`'s status.

- [x] **Step 1: Scaffold the SvelteKit project**

```bash
pnpm dlx sv create web
```

When prompted interactively, choose: **SvelteKit minimal template**, **TypeScript**, and skip add-ons (Tailwind is added manually in Step 3 so we control the v4 `@theme` setup ourselves; skip ESLint/Prettier/Vitest/Playwright for Fase 0 — they can be added in a later phase if the team wants them). If the installed `sv` CLI version exposes non-interactive flags, they may be used instead of the prompts — the goal is the same resulting project shape (TypeScript, minimal template, `web/` directory), not a specific flag spelling.

- [x] **Step 2: Install dependencies and adapter-node**

```bash
pnpm --dir web install
pnpm --dir web add -D @sveltejs/adapter-node
```

Edit `web/svelte.config.js` to import from `@sveltejs/adapter-node` instead of `@sveltejs/adapter-auto`:

```js
import adapter from '@sveltejs/adapter-node';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess(),
	kit: {
		adapter: adapter()
	}
};

export default config;
```

- [x] **Step 3: Install and wire up Tailwind v4**

```bash
pnpm --dir web add -D tailwindcss @tailwindcss/vite
```

Edit `web/vite.config.ts` to add the Tailwind plugin:

```ts
import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()]
});
```

Create `web/src/app.css`:

```css
@import 'tailwindcss';

@theme {
	/* Fase 7 fills this in with the Command Center design tokens. */
}
```

- [x] **Step 4: Import the stylesheet in the root layout**

Create or edit `web/src/routes/+layout.svelte`:

```svelte
<script lang="ts">
	import '../app.css';
	let { children } = $props();
</script>

{@render children()}
```

- [x] **Step 5: Write the placeholder page**

Replace `web/src/routes/+page.svelte` with:

```svelte
<script lang="ts">
	import { onMount } from 'svelte';

	let status = $state('checking...');

	onMount(async () => {
		try {
			const res = await fetch('/api/healthz');
			const data = await res.json();
			status = `${data.service}: ${data.status}`;
		} catch {
			status = 'unreachable';
		}
	});
</script>

<main class="flex min-h-screen items-center justify-center bg-neutral-950 text-neutral-100">
	<div class="text-center">
		<h1 class="text-3xl font-bold">TOWER</h1>
		<p class="mt-2 text-sm text-neutral-400">reactor-core health: {status}</p>
	</div>
</main>
```

- [x] **Step 6: Build to verify everything compiles**

Run: `pnpm --dir web build`
Expected: build succeeds, ends with SvelteKit's `adapter-node` output summary (a `web/build/` directory is created, no TypeScript or Svelte errors).

- [x] **Step 7: Write the Dockerfile**

```dockerfile
FROM node:lts-slim AS builder
WORKDIR /build
RUN corepack enable
COPY package.json pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY . .
RUN pnpm build

FROM node:lts-slim AS runtime
WORKDIR /app
RUN useradd --system --create-home --shell /usr/sbin/nologin tower
COPY --from=builder /build/build ./build
COPY --from=builder /build/package.json ./package.json
COPY --from=builder /build/node_modules ./node_modules
ENV NODE_ENV=production
ENV PORT=3000
USER tower
EXPOSE 3000
CMD ["node", "build"]
```

Save as `web/Dockerfile`.

- [x] **Step 8: Build and smoke-test the image standalone**

```bash
docker build -f web/Dockerfile -t tower-web:dev web
docker run --rm -d --name web-smoke -p 13000:3000 tower-web:dev
sleep 2
curl -sf http://127.0.0.1:13000/ | grep -o '<h1[^<]*</h1>'
docker stop web-smoke
```

Expected: prints something containing `TOWER` inside an `<h1>` tag, container stops cleanly. (The health status will read "unreachable" here since `/api/healthz` isn't proxied yet outside Compose — that's expected and fixed in Task 6/8.)

- [x] **Step 9: Commit**

```bash
git add web/
git commit -m "feat: scaffold SvelteKit 5 + Tailwind v4 placeholder app"
```

---

### Task 6: Docker Compose topology, Caddy edge, env template

**Files:**
- Create: `docker-compose.yml`
- Create: `Caddyfile`
- Create: `.env.example`
- Create: `.env` (local only — copied from `.env.example`, not committed)

**Interfaces:**
- Consumes: `bin/reactor-core/Dockerfile`, `bin/auth-sidecar/Dockerfile`, `web/Dockerfile` from Tasks 3-5.
- Produces: a runnable `tower-net` Compose stack — `tower-caddy` (only published port, `127.0.0.1:8080`), `tower-reactor-core`, `tower-auth-sidecar`, `tower-web`, `tower-postgres`, `tower-redis`, `tower-retention` (no-op placeholder). Task 7/8 depend on this file's exact service and container names.

- [x] **Step 1: Write `docker-compose.yml`**

```yaml
name: tower

networks:
  tower-net:
    driver: bridge

volumes:
  tower-postgres-data:
  tower-redis-data:

services:
  tower-caddy:
    image: caddy:2-alpine
    container_name: tower-caddy
    restart: unless-stopped
    ports:
      - "127.0.0.1:8080:80"
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
    networks:
      - tower-net
    depends_on:
      - tower-web
      - tower-reactor-core

  tower-reactor-core:
    build:
      context: .
      dockerfile: bin/reactor-core/Dockerfile
    container_name: tower-reactor-core
    restart: unless-stopped
    env_file:
      - .env
    networks:
      - tower-net
    depends_on:
      - tower-postgres
      - tower-redis
    healthcheck:
      test: ["CMD", "curl", "-f", "http://127.0.0.1:8081/healthz"]
      interval: 10s
      timeout: 3s
      retries: 5

  tower-auth-sidecar:
    build:
      context: .
      dockerfile: bin/auth-sidecar/Dockerfile
    container_name: tower-auth-sidecar
    restart: unless-stopped
    env_file:
      - .env
    networks:
      - tower-net
    healthcheck:
      test: ["CMD", "curl", "-f", "http://127.0.0.1:8082/healthz"]
      interval: 10s
      timeout: 3s
      retries: 5

  tower-web:
    build:
      context: ./web
      dockerfile: Dockerfile
    container_name: tower-web
    restart: unless-stopped
    environment:
      PORT: "3000"
      ORIGIN: "http://localhost:8080"
    networks:
      - tower-net
    depends_on:
      - tower-reactor-core

  tower-postgres:
    image: postgres:16
    container_name: tower-postgres
    restart: unless-stopped
    environment:
      POSTGRES_USER: tower
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:-tower_dev_only}
      POSTGRES_DB: tower
    volumes:
      - tower-postgres-data:/var/lib/postgresql/data
    networks:
      - tower-net
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U tower -d tower"]
      interval: 10s
      timeout: 3s
      retries: 5

  tower-redis:
    image: redis:7
    container_name: tower-redis
    restart: unless-stopped
    volumes:
      - tower-redis-data:/data
    networks:
      - tower-net
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 10s
      timeout: 3s
      retries: 5

  tower-retention:
    image: alpine:3
    container_name: tower-retention
    restart: "no"
    command: ["sh", "-c", "echo 'tower-retention: no-op placeholder (Fase 8 implements the real pg_cron-driven job)'; sleep 5"]
    networks:
      - tower-net
```

- [x] **Step 2: Write the Caddyfile**

```
:80 {
	handle_path /api/* {
		reverse_proxy tower-reactor-core:8081
	}

	handle {
		reverse_proxy tower-web:3000
	}
}
```

`handle_path` strips the matched `/api` prefix before proxying, so an external request to `/api/healthz` reaches `reactor-core` as `/healthz`.

- [x] **Step 3: Write `.env.example`**

```
# Postgres (Fase 0 placeholder — real secrets management arrives Fase 3)
POSTGRES_PASSWORD=tower_dev_only

# Rust logging
RUST_LOG=info

# This file grows substantially in Fase 3+ (master key path, SESSION_SECRET,
# WAHA key, VAPID keys, SPX credentials — all via envelope encryption, never
# plaintext in .env for anything beyond local Postgres dev password).
```

- [x] **Step 4: Create the local `.env` (gitignored, not committed)**

```bash
cp .env.example .env
```

- [x] **Step 5: Validate the Compose file**

Run: `docker compose config --quiet`
Expected: no output, exit code 0 (means the YAML parses and interpolates cleanly).

- [x] **Step 6: Commit**

```bash
git add docker-compose.yml Caddyfile .env.example
git commit -m "feat: add docker-compose topology, Caddy edge, env template"
```

(`.env` itself is never committed — verify with `git status --short` that it does not appear.)

---

### Task 7: CI workflow, cargo-deny config, gitleaks config

**Files:**
- Create: `.github/workflows/ci.yml`
- Create: `deny.toml`
- Create: `.gitleaks.toml`

**Interfaces:**
- Consumes: nothing new (mirrors the local commands already verified in Tasks 1-6).
- Produces: a GitHub Actions workflow that CI/branch-protection can reference by job name (`rust`, `gitleaks`).

- [x] **Step 1: Write `deny.toml`**

```toml
[graph]
targets = []

[advisories]
db-path = "~/.cargo/advisory-db"
db-urls = ["https://github.com/rustsec/advisory-db"]
yanked = "deny"

[licenses]
allow = [
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unicode-3.0",
    "Zlib",
]
confidence-threshold = 0.8

[bans]
multiple-versions = "warn"
wildcards = "deny"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
```

- [x] **Step 2: Run cargo-deny locally to catch any license/advisory issues early**

Run: `cargo deny check`
Expected: exits 0. If a dependency's license isn't in the `allow` list, add it to `deny.toml` here (only if it's genuinely a permissive license) rather than deferring the failure to CI.

- [x] **Step 3: Write `.gitleaks.toml`**

```toml
title = "tower gitleaks config"

[extend]
useDefault = true

[allowlist]
paths = [
  '''\.env\.example$''',
]
```

- [x] **Step 4: Run gitleaks locally**

Run: `gitleaks detect --source . --config .gitleaks.toml`
Expected: `no leaks found`.

- [x] **Step 5: Write `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

env:
  CARGO_TERM_COLOR: always

jobs:
  rust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust
        uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt

      - uses: Swatinem/rust-cache@v2

      - name: Build
        run: cargo build --workspace --all-targets

      - name: Test
        run: cargo test --workspace

      - name: Clippy
        run: cargo clippy --workspace --all-targets -- -D warnings

      - name: Install sqlx-cli
        run: cargo install --locked sqlx-cli --no-default-features --features rustls,postgres

      - name: sqlx prepare check
        run: cargo sqlx prepare --check --workspace
        continue-on-error: true # Fase 0 has zero SQL queries; this becomes a hard gate once Fase 2 adds real query! macros.

      - name: Install cargo-audit
        run: cargo install --locked cargo-audit

      - name: Audit
        run: cargo audit

      - name: Install cargo-deny
        run: cargo install --locked cargo-deny

      - name: Deny
        run: cargo deny check

  gitleaks:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: gitleaks/gitleaks-action@v2
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
```

The `sqlx prepare --check` step is intentionally `continue-on-error` in Fase 0 only, since there isn't a single `sqlx::query!` macro anywhere in the workspace yet to check against — remove `continue-on-error` in the Fase 2 plan once `store` has real queries and an `.sqlx/` cache is committed.

- [x] **Step 6: Commit**

```bash
git add .github/workflows/ci.yml deny.toml .gitleaks.toml
git commit -m "ci: add GitHub Actions workflow, cargo-deny and gitleaks config"
```

---

### Task 8: End-to-end verification and Fase 0 sign-off

**Files:** None created — this task only runs and records verification commands, then checks off the plan/spec.

**Interfaces:**
- Consumes: the full stack from Tasks 1-7.
- Produces: recorded command output proving the Fase 0 Definition of Done (see [`docs/superpowers/specs/2026-07-13-fase-0-scaffold-design.md`](../specs/2026-07-13-fase-0-scaffold-design.md)) is met.

- [x] **Step 1: Full workspace build/test/lint from a clean state**

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Expected: all three succeed with no errors or warnings.

- [x] **Step 2: Bring up the full stack**

```bash
docker compose up -d --build
```

Expected: all 7 services report `Created`/`Started`; no build errors.

- [x] **Step 3: Wait for health and inspect status**

```bash
sleep 15
docker compose ps
```

Expected: `tower-caddy`, `tower-web`, `tower-postgres`, `tower-redis` show `running` (or `healthy` where a healthcheck applies); `tower-reactor-core` and `tower-auth-sidecar` show `running (healthy)`; `tower-retention` shows `Exited (0)` (it's a one-shot no-op by design).

- [x] **Step 4: Verify the edge → reactor-core path**

```bash
curl -sf http://127.0.0.1:8080/api/healthz
```

Expected: `{"service":"reactor-core","status":"ok"}` (key order may vary).

- [x] **Step 5: Verify the edge → web path**

```bash
curl -sf http://127.0.0.1:8080/ | grep -o '<h1[^<]*</h1>'
```

Expected: output contains `TOWER`.

- [x] **Step 6: Verify startup logs**

```bash
docker compose logs tower-reactor-core --tail 20 | grep "reactor-core starting"
docker compose logs tower-auth-sidecar --tail 20 | grep "auth-sidecar starting"
```

Expected: both greps find a match.

- [x] **Step 7: Verify no unintended published ports**

```bash
docker compose config | grep -B5 "published:" 
docker ps --format '{{.Names}}: {{.Ports}}'
```

Expected: only `tower-caddy` shows a host port binding (`127.0.0.1:8080->80/tcp`); every other `tower-*` container shows no `0.0.0.0`/`127.0.0.1` port mapping.

- [x] **Step 8: Verify container naming**

```bash
docker compose config --services
```

Expected: exactly `tower-caddy`, `tower-reactor-core`, `tower-auth-sidecar`, `tower-web`, `tower-postgres`, `tower-redis`, `tower-retention` — no service named `api` or anything generic.

- [x] **Step 9: Clean teardown**

```bash
docker compose down
```

Expected: all containers and the `tower-net` network are removed; named volumes persist (expected — they're for Postgres/Redis data, not ephemeral).

- [x] **Step 10: Mark the Fase 0 plan and design doc complete, commit**

In `docs/superpowers/plans/2026-07-13-fase-0-scaffold.md`, check every remaining `- [ ]` box to `- [x]`.

```bash
git add docs/superpowers/plans/2026-07-13-fase-0-scaffold.md
git commit -m "docs: mark Fase 0 scaffold plan complete"
```

Fase 0 is done. Do not start Fase 1 (core-domain rule engine port) in this same pass — per the brainstorming decision, each fase gets its own spec/plan cycle, and Fase 1 needs a fresh decision about how to handle the missing `/root/projects/SPX-PORTAL` reference (see the "Catatan konteks penting" section of `docs/tower-master-spec.md`).
