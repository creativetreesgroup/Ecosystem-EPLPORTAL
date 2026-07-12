# Fase 0 — Scaffold & Fondasi (design)

Bagian dari [TOWER master spec](../../tower-master-spec.md). Lihat file itu untuk
konteks proyek penuh dan aturan keras yang berlaku di semua fase.

## Tujuan

Bentuk skeleton monorepo TOWER di root `EPL-PROJECT` (bukan repo terpisah): Cargo
workspace kosong sesuai layout arsitektur, aplikasi SvelteKit 5 placeholder, topologi
Docker Compose lokal yang benar-benar bisa `up`, dan CI dasar. **Tidak ada logika
bisnis** di fase ini — itu mulai Fase 1.

## Keputusan yang diambil

- **Tidak ada reference repo eksternal tersedia** (`/root/projects/SPX-PORTAL` tidak
  ada di mesin ini). Fase 0 tidak butuh itu sama sekali (murni scaffold), jadi tidak
  blocking. Dicatat ulang di master spec untuk Fase 1 dan seterusnya.
- Repo git baru di-`init` di root `EPL-PROJECT` (sebelumnya bukan git repo).
- Rust toolchain dipasang via `rustup` (stable channel) karena belum ada di mesin.
- CI ditulis sebagai GitHub Actions workflow (`.github/workflows/ci.yml`) — konvensi
  paling umum; tidak mengasumsikan remote tertentu, tidak menimbulkan biaya jika
  remote-nya ternyata bukan GitHub.
- Satu Cargo workspace, binary `reactor-core` dan `auth-sidecar` di `bin/`, delapan
  crate lib kosong di `crates/` (core-domain, spx-client, poller, executor, store,
  ws-hub, notifier, api-gateway) — masing-masing `lib.rs` kosong + `Cargo.toml`
  stub, tanpa dependency selain yang wajib untuk compile.
- `reactor-core` dan `auth-sidecar` di Fase 0 hanya: init tracing, log satu baris
  "starting", lalu sleep/serve minimal (reactor-core: axum health endpoint `/healthz`
  di CONTROL runtime saja — HOT runtime & business loop menyusul Fase 1+). Ini agar
  Docker Compose punya sesuatu yang benar-benar bisa health-check.
- `web/` — SvelteKit 5 (runes, `adapter-node`) + Tailwind v4 via `@theme` (tanpa token
  desain nyata, itu Fase 7), satu halaman placeholder yang fetch `/healthz` reactor-core
  untuk membuktikan wiring compose bekerja.

## Struktur direktori

```
EPL-PROJECT/
  Cargo.toml                      # workspace
  crates/
    core-domain/  spx-client/  poller/  executor/  store/  ws-hub/  notifier/  api-gateway/
  bin/
    reactor-core/
    auth-sidecar/
  web/                             # SvelteKit 5 + Tailwind v4, adapter-node
  docker-compose.yml
  Caddyfile
  .env.example
  .gitignore
  deny.toml
  .gitleaks.toml
  .github/workflows/ci.yml
  docs/
    tower-master-spec.md
    superpowers/specs/2026-07-13-fase-0-scaffold-design.md
```

## Docker Compose topologi (Fase 0)

Services, nama container eksplisit (aturan keras #8 — tidak boleh alias generik
seperti `api`):

- `tower-caddy` — edge, **satu-satunya** yang publish port, bind `127.0.0.1:8080:80`.
- `tower-reactor-core` — Rust bin, expose internal saja (network Docker), health
  `/healthz`.
- `tower-auth-sidecar` — Rust bin, expose internal saja.
- `tower-web` — SvelteKit adapter-node, expose internal saja.
- `tower-postgres` — `postgres:16`, expose internal saja, named volume.
- `tower-redis` — `redis:7`, expose internal saja.
- `tower-retention` — placeholder job container (Fase 0: no-op script; job asli
  Fase 8), tidak expose apapun.

Semua di satu dedicated network (`tower-net`), bukan default/shared network. Tidak
ada `ports:` di service manapun kecuali `tower-caddy`.

## CI (Fase 0)

`.github/workflows/ci.yml`:
- `cargo build --workspace`
- `cargo test --workspace`
- `cargo clippy --workspace -- -D warnings`
- `cargo sqlx prepare --check` (akan no-op/pass sampai ada query nyata di Fase 2;
  step ditambahkan sekarang supaya polanya established)
- `gitleaks detect`
- `cargo audit`
- `cargo deny check`

## Definition of done — Fase 0

1. `cargo build --workspace` sukses dari clean checkout.
2. `cargo clippy --workspace -- -D warnings` bersih.
3. `pnpm install && pnpm build` di `web/` sukses.
4. `docker compose config` valid; `docker compose up -d` bikin semua container
   `healthy`/`running`, dan `curl 127.0.0.1:8080` (via Caddy) berhasil mencapai
   halaman placeholder web yang menampilkan status healthz reactor-core.
5. Tidak ada published port selain edge; nama container semuanya eksplisit unik.
6. `git log` menunjukkan repo ter-init dengan commit awal berisi scaffold ini.

## Di luar cakupan (sengaja ditunda)

Semua logika bisnis, skema DB nyata, kripto, TLS-impersonation client, auth
headless, poller/executor/notifier real, route API nyata, desain UI Command Center
— seluruhnya mulai Fase 1 dan seterusnya sesuai urutan di master spec.
