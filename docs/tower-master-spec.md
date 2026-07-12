# TOWER — Master Build Prompt (engine *Reactor*)

> Sumber kebenaran (source of truth) untuk seluruh proyek TOWER. Disalin verbatim dari
> instruksi awal pengguna pada 2026-07-13. Semua spec/plan per-fase di
> `docs/superpowers/specs/` harus konsisten dengan dokumen ini. Jika ada
> ketidaksesuaian, dokumen ini yang menang kecuali pengguna menyatakan sebaliknya.

## Catatan konteks penting (disepakati saat brainstorming Fase 0)

- **Tidak ada reference repo eksternal.** Prompt asli menyebut `/root/projects/SPX-PORTAL`
  sebagai source of truth fungsional untuk logika bisnis (matching.ts, coc.ts, dst).
  Path tersebut **tidak ada** di mesin pengembangan ini (macOS, direktori kerja
  `EPL-PROJECT` kosong saat proyek dimulai). Pengguna mengonfirmasi: bangun sepenuhnya
  lokal di dalam `EPL-PROJECT`, ikuti deskripsi perilaku di master prompt ini
  apa adanya (bukan porting line-for-line dari kode acuan yang tidak dapat diakses).
  Setiap tempat di mana perilaku SPX/engine acuan diasumsikan alih-alih dibaca
  langsung dari kode, akan ditandai eksplisit di spec/plan fase terkait (utamanya
  Fase 1 dan seterusnya).
- Proyek dieksekusi **satu fase per sesi/putaran, terverifikasi** — setiap fase
  punya spec + plan + bukti verifikasi sendiri sebelum lanjut ke fase berikutnya.
- Toolchain lokal saat mulai: Node v26.4.0, pnpm 11.9.0, Docker 29.6.1 +
  Compose v5.3.0 tersedia; Rust/Cargo/rustup **tidak** terpasang (dipasang di Fase 0).

---

## PERAN & MISI

Kamu adalah senior staff engineer yang membangun **TOWER** — portal auto-accept tiket freight SPX, versi rebuild yang **jauh lebih cepat, lebih aman, dan lebih profesional** dari sistem acuan `/root/projects/SPX-PORTAL`. Fungsional **wajib 1:1 (parity penuh)** dengan acuan; yang berubah: **stack lebih agresif (Rust hot-core)**, **UI/UX baru total ("Command Center" amber)**, dan **security diperketat**. Deploy **lokal via Docker Compose dulu**, siap migrasi VPS tanpa rombak.

**Baca `/root/projects/SPX-PORTAL` sebagai source of truth fungsional.** Setiap perilaku, edge-case, dan test di sana harus punya padanan di TOWER.

### Satu batasan yang WAJIB kamu hormati (jangan bohongi diri)
Target "1ms" berlaku HANYA untuk **decision-path** (parse->match->claim->dispatch) = **<=1ms p99**. **End-to-end accept wire-bound = 30-80ms** (RTT ke SPX Singapura tak bisa dihapus bahasa apa pun). Instrumen DUA metrik terpisah: `local_dispatch_us` dan `accept_e2e_ms`. **KPI sebenarnya = win-rate.** Jangan pernah klaim sub-ms end-to-end.

---

## STACK (wajib)

| Layer | Teknologi |
|---|---|
| Hot core / API / poller / executor | **Rust** — tokio (rt-multi-thread), axum, hyper/reqwest(rustls), simd-json, serde, bytes, sqlx(postgres), deadpool-redis/redis, dashmap, tower/tower-http, tower-governor, tracing, metrics+prometheus, mimalloc/jemalloc |
| TLS-impersonation egress SPX | rquest (Chrome JA3/JA4 + H2 fingerprint) — lihat Risiko R1 |
| Auth headless (sidecar terpisah) | chromiumoxide (CDP); fallback Node Playwright di balik interface gRPC yang sama |
| Kripto | aes-gcm, hkdf, sha2, argon2, secrecy, zeroize, subtle |
| DB / cache | **PostgreSQL 16** + **Redis 7** (dipertahankan; proven) |
| Frontend | **SvelteKit 5** (runes) + **Tailwind v4** (@theme) + IndexedDB (idb), adapter-node |
| Deploy | **Docker Compose** (lokal: Caddy edge; VPS overlay: Traefik + ACME) |

---

## ARSITEKTUR (ikuti persis)

Cargo workspace, binary tunggal `reactor-core` (hot path memori bersama) + `auth-sidecar` proses terpisah:

```
crates/  core-domain (rule engine murni, NO I/O) . spx-client . poller . executor . store . ws-hub . notifier . api-gateway
bin/     reactor-core . auth-sidecar
web/     SvelteKit 5
```

**DUA Tokio runtime terpisah:**
- **HOT** (worker di-pin core_affinity): poller_task (single-flight AtomicBool CAS), notif_watcher_task (JoinSet lajur berjenjang + Semaphore + backoff), accept dispatch (in-proc claim -> warm-pool socket write). UI/DB/WS **tak boleh** mencuri siklus dari sini.
- **CONTROL:** Axum HTTP+WS, ws_bridge (Redis pub/sub->broadcast), notifier (mpsc fire-and-forget), watchdog 60s, driver_watch, retention.
- Panic isolation (JoinSet supervised + respawn), graceful shutdown (CancellationToken drain).

**Hot-path budget 1ms:** simd-json parse -> normalize_booking -> find_best_matching_rule -> in-proc atomic claim (DashMap CAS) -> build+serialize (buffer reuse, byte-patch ID) -> checkout warm conn -> socket write. **Redis Lua gate / DB write / WS / notif = async OFF critical path.**

---

## URUTAN BUILD (fase — jangan lompat)

### Fase 0 — Scaffold & fondasi
Cargo workspace + crate kosong sesuai layout. SvelteKit 5 + Tailwind v4 app. `docker-compose.yml` (caddy, reactor-core, auth-sidecar, web, postgres:16, redis:7, retention) — **no published ports kecuali edge (bind 127.0.0.1)**, nama container **unik** (`reactor-core`, JANGAN alias `api`). `.env.example`. CI: cargo build/test/clippy -D warnings, cargo sqlx prepare offline, gitleaks, cargo audit, cargo deny.

### Fase 1 — core-domain (MONEY LOGIC — port 1:1 DULU, sebelum I/O apa pun)
Port `apps/api/src/services/matching.ts` + `lib/coc.ts` ke Rust **line-for-line semantik**, lalu **port semua test** (matching.test.ts, route.test.ts, coc.test.ts) sebagai test Rust dan **buat hijau**. Wajib mempertahankan:
- 3 mode rule: booking_id (separator-tolerant, substring hanya ID>=9 char, rank tertinggi), route (whole-word locMatch -> **bali != Balikpapan / solo != Solok**, strict vs flexible, Origin=start sejati), filter.
- **Guard: route/filter rule kosong = match NOTHING** (tak pernah blanket-accept).
- Gate: bookingType, serviceTypes (vehicleMatch prefix kanonik), maxWeight, maxCodAmount, coc/nonCoc, shift/trip, minDeadlineMin.
- Ranking: mode dominan > priority > spesifisitas. find_best_matching_rule.
- COC = **prefix SPXID saja** (^\s*SPXID), cod flag TERPISAH.
- Booking-ID consumption pakai normId yang SAMA dengan matching (pelajaran insiden).
- Compile rule ke bentuk ter-precompute (decision tree/bitset) saat save, bukan evaluasi field-by-field per tiket.
- **GATE:** jangan lanjut Fase 2 sebelum SEMUA test rule engine hijau.

### Fase 2 — store + skema DB
Implement skema PRD dengan sqlx migrate (forward-only, CONCURRENTLY, NOT VALID->VALIDATE): tenants, portal_users, portal_sessions(token_hash sha256), agency_credentials(ciphertext+nonce+key_version), bookings(**is_coc GENERATED STORED**, UNIQUE(tenant_id,spx_id), raw_data jsonb), accept_rules(+route_signature generated, dedup lane), rule_booking_targets, accept_events(append-only, REVOKE UPDATE/DELETE dari app_role), notifications(SKIP LOCKED), push_subscriptions, automation_settings, site_settings, route_prices(CHECK dest 1-5), route_locations, archive_runs. Index hot-path: partial WHERE status='pending' (newest-first), covering live-list, BRIN created_at, **cursor pagination (bukan OFFSET)**. RLS tenant_id di semua tabel bisnis. Redis keys namespace per tenant_id (BUKAN DB-index).

### Fase 3 — spx-client + security kripto
- Klien SPX (rquest Chrome-impersonation) semua endpoint: bidding/list, count_v2, request/list, accept, notification count, log/list, user/list, profile. Normalisasi SpxBooking + klasifikasi retcode (taken/auth/transient/agency_dup). Korpus fixture body SPX asli sebagai test.
- **Envelope encryption:** master key (Docker secret file 0400) -> HKDF-SHA256 subkey purpose-scoped -> AES-256-GCM (nonce random per-enc, AAD bind). Tipe SecretString+zeroize (redact debug). **Tutup 3 gap acuan:** WAHA key jangan plaintext, jangan pad SESSION_SECRET, jangan reuse satu secret utk AES+HMAC. argon2id password. Session opaque 256-bit server-side (bukan JWT).

### Fase 4 — executor (3-layer dedup)
1. In-proc DashMap/AtomicBool CAS (authoritative single-node).
2. Redis Lua ACCEPT_GATE_LUA via EVALSHA (SET NX EX + kuota atomik, **fail-closed**) — **async OFF dispatch path**, durabilitas lintas-restart.
3. Durable acceptedIds (Redis ZSET 7d) di-restore **sebelum poll pertama** (bukan racing).
- Manual accept berbagi keyspace claim. **agency_dup verification** (verifyAgencyDup — fetch email acceptor op-log, banding email kita, retry 0/500/1500ms; beda->taken_by_agency+alert; inkonklusif->optimistik+unverified). Idempotent fire. Kuota re-read di dalam per-account lock (withAccountLock).

### Fase 5 — poller + notifier + ws-hub
- Poller state machine per-account: single-flight, **notif watcher** (lajur berjenjang, backoff, poke force-sweep), **fast-detect page-1** (FAST_DETECT_PAGES, aman via fetchComplete), **hedged fetch** (SWEEP_HEDGE_MS), full sweep tiap 3 cycle, priority-enrich queue, route/vehicle backfill cache, first-seen telemetry, anti-drift (resurrectPending/expireStaleBookings hanya saat sweep lengkap).
- Auto-login 3-tier (sidecar chromiumoxide->API->form), boot resume + durable primary, watchdog 60s, reactive 3x401 relogin, proaktif daily RELOGIN_DAILY_AT.
- Notifier (WAHA WhatsApp + Web Push VAPID, fire-and-forget via bus: pub/sub): notif new-ticket (dengan link "Terima cepat"), accepted, agency-loss alert, driver-assigned. Bot log bounded.
- ws-hub: channel per-session + acct:*, event lengkap, ping 30s, delta-sync ?since=.

### Fase 6 — api-gateway
Semua route acuan (auth/login/OTP-gate/sub-users/spx-creds, bookings live/history/spx-log/settings/accept, rules CRUD+sanitize+dedupe, prices publik+branding+locations, quick-accept HMAC). **OTP gate:** transisi autoAccept:false->true wajib isMainAccount + proof OTP fresh (ke nomor personal, bukan grup). RBAC require_permission terpusat. Security headers + CSP + CORS allowlist + rate-limit (tower-governor) + body-limit (carve-out 15MB branding).

### Fase 7 — Web "TOWER" Command Center
Implement design PRD **persis**: palet graphite+amber+teal (buang biru), Space Grotesk + IBM Plex Mono + Inter, dark+light (data-theme, anti-flash SSR), token Tailwind v4 @theme (no hex mentah di komponen), $lib/tokens.ts mirror canvas. 7 surface (/command default, /tickets, /rules, /price, /settings, /activity, /login). Signature: **Latency Tape** (osiloskop fosfor canvas), **Live Ticket Ticker** (virtualized, newest-first, delta-merge, "+N new"), **Rule Builder** (chip + route lane enum-bound), **Health Pills** (glyph bukan warna-saja), **Notification Center**. Realtime anti-refresh + optimistic accept + connection-lost handling. **WCAG 2.2 AA** (kontras, glyph+teks, focus 2px, keyboard penuh, target >=24px, aria-live, reduced-motion). Svelte 5 runes store, logic merge/optimistic di helper $lib teruji.

### Fase 8 — Cutover parity + observability + hardening
- **Observe-only mode:** poll+match+log would-accept dgn gate TERTUTUP terhadap snapshot staging -> diff keputusan vs engine TS pada pool sama -> arm gate HANYA setelah match **100%**.
- Dashboard SLO + alert: decision-path p50/p99, detection latency, e2e, win-rate, **false-win=0**, uptime ZSET, memory soak. Heartbeat 60s. Bench criterion decision-path di CI (fail regres >20%).
- Retention job pg_cron 30 3 * * *: **capture PK set sekali** -> archive CSV.gz+sha256 -> verify count -> **delete by captured-id set ONLY** (batch 5000) -> VACUUM. Advisory lock, DRY_RUN, archive_runs. **JANGAN derive delete dari re-run predikat waktu.**
- VPS overlay docker-compose.prod.yml: swap Caddy->Traefik+ACME, external shared network, label routing /api /auth /ws -> reactor-core, else web. **Nol perubahan kode.**

---

## ATURAN KERAS (non-negotiable)

1. **Parity dulu, optimasi kedua.** Rule engine + test hijau sebelum I/O. Observe-only diff 100% sebelum arm gate. Gate tertutup sampai terbukti.
2. **Auto-Accept Global = GLOBAL kill switch.** JANGAN pernah nyalakan tanpa izin eksplisit; rules targeted jalan sendiri saat master OFF.
3. **Fail-closed accept** bila Redis unreachable (lebih baik miss daripada double-fire).
4. **Dua metrik latensi terpisah**, jangan campur; jangan klaim sub-ms e2e.
5. **Tak ada secret plaintext** di mana pun (log, Redis, DB, .env prod). Tipe SecretString+zeroize wajib.
6. **is_coc = generated column** (self-heal by construction). COC=SPXID prefix saja; jangan fragmentasi.
7. **Delete retention by captured-id set only** (pelajaran insiden hapus-baris-salah).
8. **Nama container unik**, single-origin, no published ports kecuali edge, dedicated network (bukan shared).
9. **Anti-deteksi:** cadence login rendah/manusiawi; polling self-limiting + kill-switch; hormati ToS (akun milik user sendiri).
10. **Panic satu account tak boleh matikan proses** (JoinSet supervised + respawn).

---

## SELESAI BILA (Definition of Done)

1. Rule engine port 1:1 + **semua test acuan lulus**.
2. Observe-only diff vs engine TS = **100% match**.
3. Decision-path **<=1ms p99** terukur + alert; false-win **0**.
4. 3 gap security tertutup + gitleaks/cargo audit/cargo deny hijau.
5. UI TOWER lengkap 7 surface, WCAG 2.2 AA, dark+light.
6. `docker compose up` lokal end-to-end jalan; overlay VPS Traefik tanpa perubahan kode.
7. Soak 7-hari uptime >=99.9%, memory flat.
