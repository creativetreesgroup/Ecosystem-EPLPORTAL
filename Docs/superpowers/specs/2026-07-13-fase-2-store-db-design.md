# Fase 2 — store + skema DB (design)

Bagian dari [TOWER master spec](../../tower-master-spec.md). Cakupan: 14 tabel + RLS +
index hot-path + crate `store` (koneksi pool, migration runner, tipe Rust per-tabel).

## Sumber kebenaran — dan catatan penting soal itu

**Tidak ada "skema PRD" terpisah di mana pun.** Riset menyeluruh terhadap
`/tmp/spx-portal-ref` (lihat catatan di bawah) mengonfirmasi: repo acuan **tidak
multi-tenant** (satu deployment = satu agency SPX, isolasi lewat deployment
terpisah + Redis DB index berbeda, bukan `tenant_id`), skema SQL aktualnya cuma
**6 tabel** (`sessions, bookings, route_prices, route_locations, site_settings,
activity_log`, didefinisikan via raw SQL di `apps/api/src/lib/db.ts`'s
`migrate()`, BUKAN di `schema.ts` Drizzle yang sedikit basi), dan sebagian besar
state (rule engine per-account, sub-user, kredensial SPX terenkripsi, push
subscription, config bot WAHA, log login) **cuma hidup di Redis, tidak pernah
masuk Postgres**.

Jadi Fase 2 di sini **bukan porting 1:1** seperti Fase 1 — ini implementasi skema
BARU sesuai spesifikasi eksplisit master spec sendiri (yang memang secara sengaja
minta multi-tenant + RLS + generated column + audit trail immutable, hal-hal yang
tidak ada di acuan). Riset repo acuan dipakai untuk **memastikan tidak ada
perilaku fungsional yang hilang** (field apa saja yang benar-benar dibaca/ditulis,
generated column tersembunyi yang cuma ada lewat raw SQL, dll) — bukan untuk
meniru struktur tabelnya.

### Temuan riset yang memengaruhi desain

- `bookings` di acuan punya kolom generated **tersembunyi** yang tidak ada di
  `schema.ts`: `needs_enrichment boolean GENERATED ALWAYS AS (...) STORED`,
  dipakai sebagai predicate partial-index untuk query gap-enrichment poller.
  **Diadopsi** — lihat skema `bookings` di bawah.
- `is_coc` di acuan sudah py generated-column-equivalent (SQL predicate
  `IS_COC_SQL` di `coc.ts`): `(spx_id ~* '^SPXID' OR COALESCE(raw_data->>'booking_name','') ~* '^SPXID')`
  — **dipakai verbatim** sebagai definisi `GENERATED ALWAYS AS (...) STORED`
  untuk `is_coc`, konsisten dengan Fase 1's `is_coc_name`/`is_coc` di Rust (Aturan
  Keras #6: "self-heal by construction").
- Kredensial SPX di acuan **sudah** dienkripsi AES-256-GCM (di Redis, satu blob
  global) — pola envelope encryption Fase 3 tinggal memindahkan ini ke Postgres
  (`agency_credentials`, ciphertext+nonce+key_version) dan membuatnya per-tenant.
- Konsep "sub-user berbagi satu akun SPX" + flag `isMainAccount` di acuan
  (Redis-only) **diadopsi** ke `portal_users` (kolom `is_main_account`), scoped
  per-tenant.
- `AcceptRule` acuan (jsonb blob di dalam `sessions.accept_rules`) **dipecah**
  jadi tabel relasional `accept_rules` dengan kolom asli (bukan jsonb) — field
  persis sama dengan tipe Rust `AcceptRule`/`RuleConditions` dari Fase 1's
  `core-domain` (lihat `Backend/crates/core-domain/src/rule.rs`), supaya `store`
  dan `core-domain` tidak fragmentasi definisi.
- `activity_log` acuan (16 nilai `type` distinct ditemukan via riset) —
  **tidak** di-port sebagai tabel terpisah di Fase 2; fungsinya tumpang tindih
  dengan `accept_events` (append-only) untuk keputusan accept, dan log
  aktivitas umum lain (login, settings, dll) ditunda ke fase yang benar-benar
  butuh (Fase 6 api-gateway) supaya tidak membangun tabel yang belum dipakai
  (YAGNI) — `accept_events` di Fase 2 fokus HANYA pada keputusan accept/reject
  per booking, sesuai cakupan eksplisit master spec.
- `site_settings` acuan cuma pakai SATU key nyata (`price_page`, isi `Branding`)
  — desain key-value generik tetap dipertahankan (master spec eksplisit minta
  ini), tapi tidak di-hardcode ke satu key saja.
- Redis key namespace per-tenant ("BUKAN DB-index") — **bukan cakupan Fase 2**
  (itu `store`'s Redis-facing bagian, menyusul saat executor/poller Fase 4-5
  butuh Redis nyata). Fase 2 murni Postgres + tipe Rust.

## Prinsip desain

1. **Multi-tenant dari akar**: `tenants` adalah tabel akar; setiap tabel bisnis
   punya `tenant_id UUID NOT NULL REFERENCES tenants(id)` + RLS policy yang
   membatasi ke `current_setting('app.tenant_id')::uuid`.
2. **Forward-only migration**: `sqlx migrate add`, tidak pernah edit migration
   lama. Index besar via `CREATE INDEX CONCURRENTLY` (di luar migration
   transaction — sqlx mendukung ini lewat annotasi `-- no-transaction` di
   header file migration). Constraint yang berpotensi lock lama:
   `NOT VALID` dulu lalu `VALIDATE CONSTRAINT` di migration terpisah — untuk
   Fase 2 (skema kosong, belum ada data), ini lebih soal menetapkan
   **konvensi** yang dipakai konsisten mulai sekarang, bukan kebutuhan teknis
   mendesak (tabel kosong = lock instan tidak masalah).
3. **`store` crate = akses tipe-aman ke Postgres**, TIDAK berisi logika bisnis
   (itu tetap di `core-domain`/`executor`/dll). Fase 2 scope: connection pool
   (`sqlx::PgPool`), migration runner, DAN tipe Rust 1:1 per tabel (row
   structs) — repository/query function per use-case ditunda ke fase yang
   benar-benar memakainya (Fase 3 utk `agency_credentials`, Fase 4 utk
   `accept_events`, dst) supaya `store` tidak membangun API yang belum ada
   pemanggilnya (YAGNI, sama prinsipnya dengan kenapa `activity_log` ditunda).
4. **`accept_rules` bukan jsonb blob** — kolom relasional persis field
   `RuleConditions` Fase 1, plus `route_signature` generated column (dari
   origin+destinations+match_mode+booking_type+service_types dinormalisasi,
   sesuai algoritma `dedupeRules`'s lane signature di `matching.ts`/`rule.rs`)
   untuk dedup lane di level DB (partial unique index), bukan cuma di kode.

## Skema — 14 tabel

Tipe umum: `id UUID PRIMARY KEY DEFAULT gen_random_uuid()` kecuali disebut lain.
Semua tabel bisnis (semua kecuali `tenants` sendiri) punya
`tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE` + RLS ON.

### 1. `tenants`
`id, name text NOT NULL, slug text UNIQUE NOT NULL, created_at timestamptz NOT NULL DEFAULT now()`

### 2. `portal_users`
`id, tenant_id, username text NOT NULL, password_hash text NOT NULL` (argon2id,
Fase 3), `display_name text NOT NULL, is_main_account boolean NOT NULL DEFAULT
false, enabled boolean NOT NULL DEFAULT true, created_at, updated_at`.
`UNIQUE(tenant_id, username)`.

### 3. `portal_sessions`
Opaque 256-bit server-side session (Fase 3: "bukan JWT"). `id, tenant_id,
portal_user_id UUID NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE,
token_hash bytea NOT NULL` (sha256 dari token asli — token asli TIDAK PERNAH
disimpan, sesuai Aturan Keras #5), `ip inet, user_agent text, created_at,
expires_at timestamptz NOT NULL, last_seen_at timestamptz NOT NULL DEFAULT
now()`. `UNIQUE(token_hash)`. Index: `(portal_user_id)`, partial
`WHERE expires_at > now()`.

### 4. `agency_credentials`
Kredensial SPX terenkripsi (envelope encryption Fase 3). `id, tenant_id, label
text NOT NULL` (nama akun/agency), `username text NOT NULL, ciphertext bytea
NOT NULL, nonce bytea NOT NULL, key_version int NOT NULL, created_at,
updated_at`. `UNIQUE(tenant_id, label)`.

### 5. `bookings`
`id, tenant_id, spx_id text NOT NULL, raw_data jsonb NOT NULL,
status varchar(32) NOT NULL DEFAULT 'pending', is_coc boolean GENERATED ALWAYS
AS (spx_id ~* '^SPXID' OR COALESCE(raw_data->>'booking_name','') ~* '^SPXID')
STORED, needs_enrichment boolean GENERATED ALWAYS AS (
  raw_data->>'route_detail_list' IS NULL AND raw_data->>'route_stops' IS NULL
) STORED` (predicate diadaptasi dari acuan `db.ts` — field yang perlu enrichment
lanjutan), `service_type text, weight real NOT NULL DEFAULT 0, cod_amount real
NOT NULL DEFAULT 0, auto_accepted boolean NOT NULL DEFAULT false,
accept_latency_ms int, rule_matched uuid REFERENCES accept_rules(id),
created_at, updated_at`. `UNIQUE(tenant_id, spx_id)`.
Index hot-path: partial `WHERE status='pending'` (newest-first, `created_at
DESC`), covering index untuk live-list query (`status, created_at, id` INCLUDE
kolom yang sering dipakai UI), BRIN pada `created_at` (tabel besar,
append-mostly, BRIN jauh lebih murah dari B-tree untuk range-scan waktu).

### 6. `accept_rules`
Kolom persis `RuleConditions` Fase 1 (`Backend/crates/core-domain/src/rule.rs`),
bukan jsonb: `id, tenant_id, name text NOT NULL, enabled boolean NOT NULL
DEFAULT false, priority int NOT NULL DEFAULT 0, mode text NOT NULL` (CHECK IN
`booking_id,route,filter`), `service_types text[] NOT NULL DEFAULT '{}',
max_weight real, coc_only boolean NOT NULL DEFAULT false, non_coc_only boolean
NOT NULL DEFAULT false, max_cod_amount real, origin text NOT NULL DEFAULT '',
destinations text[] NOT NULL DEFAULT '{}' CHECK (array_length(destinations,1)
IS NULL OR array_length(destinations,1) <= 5), booking_type text NOT NULL
DEFAULT 'all'` (CHECK IN `spxid,reguler,all`), `shift_types int[] NOT NULL
DEFAULT '{}', trip_types int[] NOT NULL DEFAULT '{}', match_mode text NOT NULL
DEFAULT 'strict'` (CHECK IN `strict,flexible`), `min_deadline_min int,
max_accept_count int NOT NULL DEFAULT 0, accepted_count int NOT NULL DEFAULT
0, route_signature text GENERATED ALWAYS AS (
  lower(regexp_replace(origin, '[^a-zA-Z0-9]+', ' ', 'g')) || '|' ||
  array_to_string(destinations, '>') || '|' || match_mode || '|' ||
  booking_type
) STORED` (dedup-lane signature — versi SQL dari lane signature `dedupeRules`
di `matching.ts`; normalisasi destinasi penuh ala `norm_loc` ditunda ke
aplikasi layer karena regex SQL tidak semudah kode Rust untuk itu — signature
ini "cukup baik" untuk index dedup, normalisasi presisi tetap terjadi di
`core-domain` sebelum insert), `created_at, updated_at`. Partial unique index
utk dedup lane: `UNIQUE(tenant_id, route_signature) WHERE mode='route'`.

### 7. `rule_booking_targets`
Target booking_id-mode, dipisah dari `accept_rules` (list ID bisa banyak, perlu
dedup sendiri ala `dedupeRules`'s ID-claim). `id, tenant_id, rule_id UUID NOT
NULL REFERENCES accept_rules(id) ON DELETE CASCADE, booking_id_raw text NOT
NULL` (string asli, dipertahankan apa adanya — dikembalikan oleh
`matched_booking_id_for`), `booking_id_norm text NOT NULL` (hasil `norm_id`,
Fase 1), `created_at`. `UNIQUE(tenant_id, booking_id_norm)` (satu ID cuma
diklaim satu rule aktif — mirror `dedupeRules`'s enabled-first-claim, tapi
di-enforce di DB level saat insert, bukan cuma re-derive tiap load).

### 8. `accept_events`
Append-only audit trail keputusan accept (Aturan Keras: append-only,
`REVOKE UPDATE, DELETE ON accept_events FROM app_role` — role `app_role`
dibuat di migration ini, dipakai aplikasi runtime; role migrasi/admin superuser
tetap bisa, tapi app biasa tidak). `id, tenant_id, booking_id UUID REFERENCES
bookings(id), rule_id UUID REFERENCES accept_rules(id), outcome text NOT
NULL` (CHECK IN `accepted,rejected,skipped,taken_by_agency,failed,
agency_dup_unverified`), `local_dispatch_us bigint, accept_e2e_ms bigint,
detail jsonb NOT NULL DEFAULT '{}', created_at timestamptz NOT NULL DEFAULT
now()`. Index: `(tenant_id, created_at DESC)`, BRIN `created_at`.

### 9. `notifications`
Antrian outbound (WhatsApp/push), didesain utk worker claim via
`SELECT ... FOR UPDATE SKIP LOCKED`. `id, tenant_id, channel text NOT NULL`
(CHECK IN `whatsapp,push`), `payload jsonb NOT NULL, status text NOT NULL
DEFAULT 'pending'` (CHECK IN `pending,sent,failed`), `attempts int NOT NULL
DEFAULT 0, created_at, sent_at timestamptz`. Partial index
`WHERE status='pending'` (untuk `SKIP LOCKED` claim query cepat).

### 10. `push_subscriptions`
`id, tenant_id, portal_user_id UUID NOT NULL REFERENCES portal_users(id) ON
DELETE CASCADE, endpoint text NOT NULL, p256dh text NOT NULL, auth text NOT
NULL, created_at, expires_at timestamptz NOT NULL` (mirror TTL 30-hari acuan,
tapi durable). `UNIQUE(tenant_id, endpoint)`.

### 11. `automation_settings`
Satu baris per tenant (bukan per portal_user — "Auto-Accept Global" adalah
kill switch tenant-wide per Aturan Keras #2). `tenant_id UUID PRIMARY KEY
REFERENCES tenants(id) ON DELETE CASCADE, auto_accept_enabled boolean NOT
NULL DEFAULT false, poll_interval_ms int NOT NULL DEFAULT 1000, smart_paused
boolean NOT NULL DEFAULT false, smart_paused_until timestamptz,
smart_dry_run boolean NOT NULL DEFAULT false, smart_schedule jsonb NOT NULL
DEFAULT '{}', smart_blacklist text[] NOT NULL DEFAULT '{}',
counter_reset_hour int, counter_reset_last_at timestamptz, updated_at`.
**Default `auto_accept_enabled = false` adalah enforcement Aturan Keras #2 di
level skema** — tidak ada cara membuat baris baru dengan kill switch aktif
tanpa eksplisit.

### 12. `site_settings`
`tenant_id, key text NOT NULL, value jsonb NOT NULL, updated_at`.
`PRIMARY KEY(tenant_id, key)`.

### 13. `route_prices`
`id, tenant_id, route_code text NOT NULL, region text NOT NULL DEFAULT '',
origin text NOT NULL, destinations jsonb NOT NULL` (array string, 1–5 item —
**CHECK dest 1-5** eksplisit dari master spec: `CHECK (jsonb_array_length(destinations)
BETWEEN 1 AND 5)`), `price bigint NOT NULL, vehicle_type text NOT NULL,
created_at, updated_at`. `UNIQUE(tenant_id, route_code)`.

### 14. `route_locations`
`id, tenant_id, name text NOT NULL, created_at`. `UNIQUE(tenant_id, name)`.

### 15. `archive_runs`
(15 tabel total — nama "14 tabel" di ringkasan cakupan master spec tidak
termasuk `archive_runs` secara eksplisit di listing tapi disebutkan sebagai
bagian retention job Fase 8; didefinisikan sekarang supaya skema-nya utuh
sejak awal, kosong/tidak dipakai sampai Fase 8). Sistem-wide, tidak
tenant-scoped (retention adalah operasi maintenance lintas-tenant): `id,
table_name text NOT NULL, run_at timestamptz NOT NULL DEFAULT now(),
captured_count bigint NOT NULL, archived_count bigint NOT NULL,
deleted_count bigint NOT NULL, archive_path text, sha256 text, status text
NOT NULL DEFAULT 'running'` (CHECK IN `running,completed,failed`),
`dry_run boolean NOT NULL DEFAULT false`. Tanpa RLS (bukan tabel bisnis
per-tenant).

## Cursor pagination (bukan OFFSET)

Query live-list bookings TIDAK pakai `OFFSET` (mahal pada tabel besar, dan
tidak stabil kalau baris baru masuk saat scroll). Konvensi: cursor = tuple
`(created_at, id)` terakhir dilihat, query lanjutan pakai `WHERE (created_at,
id) < ($1, $2) ORDER BY created_at DESC, id DESC LIMIT $3` — cocok dengan
covering index di atas. Ini pola query yang dipakai `store`'s fungsi
list-booking mulai fase yang membutuhkannya (bukan dibangun sekarang, hanya
index-nya yang disiapkan sekarang supaya query itu murah begitu ditulis).

## RLS

Semua tabel bisnis (semua kecuali `archive_runs`): `ALTER TABLE ... ENABLE ROW
LEVEL SECURITY;` + `CREATE POLICY tenant_isolation ON ... USING (tenant_id =
current_setting('app.tenant_id')::uuid);`. Aplikasi (`store` crate) WAJIB
`SET LOCAL app.tenant_id = '<uuid>'` di awal setiap transaksi request — ini
bagian dari `store`'s connection-acquire helper, bukan diulang manual di
setiap query call site.

## `store` crate — cakupan Fase 2

```
Backend/crates/store/
  Cargo.toml            (sqlx dengan fitur postgres, runtime-tokio-rustls,
                         uuid, chrono/time, macros; deadpool-redis DITUNDA ke
                         Fase 4, store Fase 2 murni Postgres)
  migrations/            (sqlx migrate — 1 file per tabel/kelompok terkait,
                         forward-only)
  src/
    lib.rs                (re-export publik)
    pool.rs                 (fn connect(database_url) -> PgPool, fn
                            with_tenant(&pool, tenant_id) -> transaksi yang
                            sudah SET LOCAL app.tenant_id)
    models/                  (satu file per tabel/kelompok, row struct
                            `#[derive(sqlx::FromRow)]` 1:1 kolom — TIDAK ada
                            repository function di sini, cuma tipe)
      tenant.rs, portal_user.rs, portal_session.rs, agency_credential.rs,
      booking.rs, accept_rule.rs, rule_booking_target.rs, accept_event.rs,
      notification.rs, push_subscription.rs, automation_settings.rs,
      site_settings.rs, route_price.rs, route_location.rs, archive_run.rs
```

## Verifikasi Fase 2

Karena ini skema baru (bukan port logika dengan test acuan seperti Fase 1),
verifikasi utamanya: (1) `sqlx migrate run` sukses dari kosong ke skema penuh
di Postgres 16 nyata (via `Docker/docker-compose.yml`'s `tower-postgres`,
sudah ada sejak Fase 0), (2) `cargo sqlx prepare` sukses (query
compile-time-checked terhadap skema nyata — mengaktifkan CI check yang sejak
Fase 0 di-`continue-on-error` karena belum ada query), (3) smoke test: insert
1 baris ke tiap tabel via `store`'s row struct, select balik, assert field
sama (round-trip), di dalam transaksi tenant yang benar (RLS harus lolos) DAN
gagal dengan benar kalau tenant salah (RLS harus block), (4) generated column
`is_coc`/`needs_enrichment`/`route_signature` diuji dengan insert row nyata
dan assert nilai generated-nya benar, dicocokkan manual terhadap Fase 1's
`is_coc`/`is_coc_name` Rust logic untuk beberapa kasus yang sama (double-check
SQL predicate == Rust predicate, sesuai peringatan "self-heal by construction"
Aturan Keras #6).

## Definition of Done — Fase 2

1. 15 tabel + RLS + index (partial/covering/BRIN) diterapkan lewat
   `sqlx migrate`, forward-only, sesuai konvensi CONCURRENTLY/NOT VALID.
2. `is_coc` generated column diverifikasi identik secara semantik dengan Fase
   1's `is_coc_name`/`is_coc` (kasus uji yang sama, hasil sama) — bukti
   konkret, bukan asumsi.
3. `accept_events` immutable: `UPDATE`/`DELETE` oleh `app_role` gagal
   (dibuktikan lewat test yang benar-benar mencoba dan mengharapkan error).
4. `automation_settings.auto_accept_enabled` default `false` di skema
   (Aturan Keras #2 di level DB, bukan cuma konvensi aplikasi).
5. `route_prices.destinations` CHECK 1-5 item diverifikasi (insert 0 dan 6
   item harus ditolak DB, bukan cuma divalidasi di aplikasi).
6. `store` crate compile bersih, `cargo sqlx prepare` sukses, smoke test
   round-trip semua 15 tabel hijau, RLS cross-tenant block diverifikasi.
7. `cargo clippy -p store -- -D warnings` bersih.
