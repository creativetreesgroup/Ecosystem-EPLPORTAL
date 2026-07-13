# Fase 5 — poller + notifier + ws-hub (design)

Bagian dari [TOWER master spec](../../tower-master-spec.md). Sumber acuan fungsional:
`/tmp/spx-portal-ref`. File acuan persis: `apps/api/src/services/poller.ts` (2535 baris —
sudah dibaca dua kali, sebagian untuk riset Fase 4's dedup/executor, sekarang untuk sisa
tanggung jawab poller), `apps/api/src/services/spx-auth.ts`, `apps/api/src/services/
spx-browser.ts`, `apps/api/src/services/webhook.ts`, `apps/api/src/services/push.ts`,
`apps/api/src/ws/hub.ts`, `apps/api/src/lib/db.ts` (`resurrectPending`/`expireStaleBookings`),
`apps/api/src/routes/bookings.ts` (delta-sync `/live?since=`).

## Ini fase terbesar sejauh ini — 3 crate sekaligus

Judul Fase 5 di master spec ("poller + notifier + ws-hub") memang menggabungkan 3 crate
yang sudah di-scaffold sejak Fase 0. Karena cakupannya jauh lebih besar dari fase
sebelumnya (state machine polling, browser automation, WebSocket server, pub/sub bridge),
plan implementasinya diperkirakan >12 task — dipecah dengan batas tugas yang lebih ketat
dari fase-fase sebelumnya, bukan berarti fase ini dipecah jadi beberapa "Fase 5a/5b" (itu
akan menyimpang dari urutan fase yang didikte master spec).

## Koreksi terhadap deskripsi master spec (temuan riset — banyak, baca teliti)

1. **`SPX_FAST_DETECT_PAGES` dan `SPX_SWEEP_HEDGE_MS` default `0` (MATI)** di acuan — ini
   knob opt-in performa, BUKAN mekanisme yang selalu aktif seperti tersirat di master spec.
   Port Rust: sama, default off, dikonfigurasi via env var, dengan test yang membuktikan
   perilaku default (mati) DAN perilaku ketika diaktifkan.
2. **Tier-1 auto-login acuan BUKAN sidecar** — acuan pakai Playwright headless Chromium
   in-process singleton (`spx-browser.ts`), bukan proses terpisah. **Master spec DAN Fase 0
   scaffold repo ini SUDAH mendikte arsitektur proses terpisah** (`bin: reactor-core .
   auth-sidecar` — `auth-sidecar` sudah ada sejak Fase 0 khusus untuk ini). Jadi di sini
   desain Rust **sengaja menyimpang dari acuan** (bukan port literal) untuk mengikuti
   arsitektur proses yang SUDAH didikte lebih dulu: `poller` (jalan di `reactor-core`)
   memanggil `auth-sidecar` (proses terpisah, sudah pegang `chromiumoxide`) lewat HTTP
   internal untuk tier-1 browser-login, bukan meng-embed browser automation langsung ke
   proses hot-path. Rasional: panic/crash browser automation (chromiumoxide/Chromium
   process) TIDAK BOLEH menjatuhkan proses hot-path yang memegang seluruh dedup
   in-proc/quota lock (Aturan Keras #10 — panic isolation), dan browser automation
   punya footprint memori/CPU yang tidak seharusnya bersaing dengan hot-path 1ms budget.
3. **Notif watcher "lajur berjenjang" (spec) = staggered parallel lanes (realitas)** —
   bukan multi-tier interval berbeda, tapi SATU interval dengan N lane paralel
   (`SPX_NOTIF_WATCH_CONCURRENCY`, default 2) yang saling overlap. Backoff eksponensial
   ×2, floor 250ms, cap 5000ms, reset ke 0 begitu satu tick sehat. Port persis.
4. **Watchdog 60s acuan = in-process, HANYA untuk akun "durable primary"** (satu akun
   spesial dari `PORTAL_USERNAME`) — bukan health-check semua akun. Key Redis
   `spx:poller_heartbeat:<acct>` ditulis acuan untuk "watchdog eksternal" yang **TIDAK
   PERNAH ADA** di repo acuan (dead code aspirational). Port Rust: implementasikan
   watchdog in-process 60s yang benar-benar dipakai (recreate durable-primary poller kalau
   hilang), TETAP tulis heartbeat key (siapa tahu Fase 8's observability nanti benar-benar
   konsumsi itu — tapi jangan bangun consumer-nya sekarang, itu bukan scope Fase 5).
5. **Reactive relogin "3x401" nyata** (`consecutive401s >= 3`), tapi jalur accept
   langsung "lompat" ke threshold (`max(consecutive401s, 3)`) begitu accept gagal karena
   auth — bukan menunggu 3 kegagalan poll terpisah. Port persis kedua jalur ini.
6. **Notifier "fire-and-forget via bus: pub/sub" TIDAK AKURAT** — acuan TIDAK punya
   pub/sub internal untuk notifier. Notifier = HTTP fire-and-forget langsung ke WAHA
   (`POST {waha_url}/api/sendText`) + opsional n8n webhook + Web Push VAPID
   (`web-push`-equivalent). Satu-satunya pub/sub nyata di sistem acuan adalah ws-hub
   (poin 8). Desain ini mengikuti REALITAS acuan: `notifier` crate = HTTP client
   fire-and-forget (spawn task, log error, jangan propagate), TANPA layer bus buatan.
7. **Delta-sync `?since=` BUKAN bagian WebSocket** — itu param REST di endpoint
   `/live` (Fase 6's api-gateway, bukan Fase 5's ws-hub). ws-hub Fase 5 hanya mengurus
   push event real-time (`tickets_removed` dst.); katch-up setelah reconnect untuk baris
   yang BERUBAH (bukan dihapus) adalah tanggung jawab REST Fase 6. **Di luar cakupan
   Fase 5** — dicatat eksplisit supaya tidak disangka hilang.
8. **ws_bridge (Redis pub/sub → broadcast) AKURAT** — ini satu-satunya bagian arsitektur
   WS di master spec yang sesuai realitas 1:1. Channel key = per-session DAN per-akun
   (`acct:<id>`, bukan wildcard `acct:*` seperti tersirat spec — channel spesifik per
   akun, `ws-hub` subscribe semua channel Redis yang relevan lalu deliver ke socket lokal
   yang cocok). Ping 30s dikonfirmasi persis.
9. **`resurrectPending`/`expireStaleBookings` HANYA jalan saat `fetchComplete`** — bukan
   sekadar preferensi performa, ini GATE KOREKTNES: sweep parsial (halaman gagal sebagian,
   fast-detect, window berputar) tidak boleh dipakai basis "tiket mana yang hilang dari
   pool", karena itu akan salah-expire tiket yang sebenarnya masih hidup (insiden nyata
   yang dicatat komentar acuan — "REG only 500 of 1146"). Port Rust WAJIB gate yang sama:
   fungsi anti-drift hanya dipanggil kalau `fetch_complete` true.

## Tujuan

Bangun 3 crate: `poller` (state machine per-akun: single-flight, notif watcher, fast-detect,
hedged fetch, full-sweep-tiap-3-siklus, anti-drift, auto-login orkestrasi tier 2/3 +
panggil `auth-sidecar` untuk tier 1, watchdog durable-primary), `notifier` (WAHA + Web Push
VAPID, fire-and-forget), `ws-hub` (WebSocket server, channel per-sesi+per-akun, ping 30s,
Redis pub/sub bridge). Ketiganya dikonsumsi Fase 6 (api-gateway) sebagai library/service
yang di-mount ke `reactor-core`.

## Keputusan arsitektur

### Satu Tokio task per akun, bukan satu event-loop

Acuan JS single-threaded punya SATU event loop menangani semua akun via `setTimeout`
self-rescheduling. Rust port: SATU `tokio::task` per akun aktif (map akun→`JoinHandle`,
mirip `pollers: Map<accountId, PollerState>` acuan tapi task Tokio sungguhan, bukan hanya
struct state). Loop dalam task: `loop { poll_once(...).await; tokio::select! { _ =
sleep(interval) => {}, _ = poke_notify.notified() => {} } }` — `poke_notify` adalah
`tokio::sync::Notify` per akun yang notif-watcher pakai untuk membangunkan task lebih awal
(port `pokePoll`'s "reschedule dalam 1ms" — di Tokio ini `notify_one()` yang membatalkan
`sleep` yang sedang jalan via `select!`, bukan re-timer 1ms).

Single-flight otomatis benar by construction: karena hanya SATU task per akun yang
menjalankan `poll_once` secara berurutan (loop, bukan spawn baru tiap siklus), tidak
mungkin dua siklus untuk akun yang sama tumpang tindih — properti yang acuan harus jaga
manual via flag `state.polling` sekarang didapat gratis dari struktur task Tokio-nya
sendiri. Notif watcher tetap task terpisah (per akun) yang HANYA membaca 2 counter
endpoint ringan dan memanggil `poke_notify.notify_one()` — tidak pernah menyentuh
`AccountDedupState`/executor langsung.

### Auto-login: tier 2/3 in-proc, tier 1 via HTTP ke `auth-sidecar`

`poller` crate TIDAK depend ke `chromiumoxide`/browser-automation apa pun. Tier 1 (browser
login) dipanggil via HTTP internal: `POST http://auth-sidecar:8082/login {account_id,
credentials_ref}` → `auth-sidecar` (proses Fase 0 terpisah, port 8082) yang nanti — di
task terkait — akan pegang `chromiumoxide` dan kembalikan cookies SPX hasil login browser
sebagai JSON. **Fase 5 TIDAK membangun isi `auth-sidecar`'s browser-automation itu
sendiri** (itu scope yang belum ditentukan — kemungkinan besar task tersendiri di Fase 5
juga, karena `auth-sidecar` bin sudah ada tapi kosong sejak Fase 0, dan tier-1 login MEMANG
bagian dari "Auto-login 3-tier" yang eksplisit diminta master spec Fase 5). `poller` hanya
perlu tahu KONTRAK HTTP-nya (request/response shape) — implementasi `auth-sidecar`'s
handler + `chromiumoxide` automation ITU SENDIRI adalah task terpisah dalam plan Fase 5
ini (bukan crate `poller`, tapi `bin/auth-sidecar`), supaya pemisahan proses yang jadi
rasional keputusan #2 di atas benar-benar terwujud, bukan cuma niat di desain.

Tier 2 (API login) dan tier 3 (form login) di-port ke `poller` (atau modul yang dipakai
`poller`) sebagai panggilan `spx-client` HTTP biasa — tidak butuh browser, aman di
hot-path proses `reactor-core`.

Urutan coba tetap 1→2→3 (tier 1 dulu — port PERSIS urutan acuan, walau implementasi
tier-1-nya beda proses). Kalau `auth-sidecar` tidak reachable (proses belum jalan/down),
`poller` fallback ke tier 2 — TIDAK boleh gagal keseluruhan hanya karena sidecar down,
karena tier 2/3 memang ada persis untuk skenario itu di acuan.

### Watchdog durable-primary — port perilaku nyata, bukan deskripsi spec

`primary_account_id()` dari config (analog `PORTAL_USERNAME` acuan). Watchdog: satu
`tokio::task` global (bukan per-akun) dengan `interval(60s)`, memanggil
`ensure_durable_poller_alive()` yang — kalau task poller utk akun primary tidak ada/mati —
membuat ulang (baca cookies tersimpan atau trigger auto-login penuh). Heartbeat Redis
`spx:poller_heartbeat:<acct>` tetap ditulis tiap siklus (untuk observability Fase 8 nanti),
TAPI tidak ada consumer yang dibangun sekarang (sesuai realitas acuan — jangan bangun
fitur yang "kelihatan lengkap" padahal setengah, itu melanggar YAGNI).

### Anti-drift: gate `fetch_complete` sebagai tipe, bukan boolean lepas

`FetchOutcome { fetch_complete: bool, spx_id_set: HashSet<String>, page_failures: u32, ...
}` — hasil sweep dibungkus tipe eksplisit, bukan boolean terpisah yang gampang lupa dicek.
`resurrect_pending`/`expire_stale_bookings` (nama fungsi port persis acuan) MENERIMA
`FetchOutcome`, bukan `HashSet<String>` mentah — signature-nya sendiri memaksa caller
membuktikan sudah lewat gate `fetch_complete` (kalau caller punya `HashSet` mentah tanpa
`FetchOutcome`, kode tidak akan compile) — desain "invalid state tidak representable"
yang sudah jadi pola proyek ini sejak Fase 1.

### `notifier`: fire-and-forget murni, tanpa bus buatan

`notifier` crate: `pub async fn notify_new_ticket(...)`, `notify_accepted(...)`,
`notify_agency_loss(...)` (dipanggil Fase 4's `AgencyDupOutcome::LostToAgency` di layer
atasnya — `notifier` sendiri TIDAK tahu tentang `executor`, cuma terima data event murni),
`notify_driver_assigned(...)` — semua langsung `spx_client`-style HTTP call (WAHA
`sendText`, opsional n8n webhook) + Web Push VAPID (`web-push`-equivalent crate Rust, cari
yang aktif dipelihara & lisensi cocok saat implementasi — riset versi nyata, bukan tebak).
Caller (poller/executor-consuming layer) `tokio::spawn`-kan panggilan ini dan buang
`Result`-nya (log error via `tracing::warn!`, jangan `?` — kegagalan notifikasi TIDAK
BOLEH menggagalkan accept yang sudah sukses).

### `ws-hub`: axum WS + Redis pub/sub bridge

Channel key sama seperti acuan: per-`session_id` DAN `acct:<account_id>` (lowercased).
`DashMap<String, HashSet<...WebSocket sink...>>` untuk koneksi lokal proses ini + Redis
`SUBSCRIBE` terpisah yang deliver ke koneksi lokal yang cocok channel-nya (port
`redisSub`/`setupSub` acuan). Ping 30s via `tokio::time::interval`. Event union type
(`WsEvent` enum, `#[serde(tag="type")]`) mencakup semua varian yang acuan kirim
(`new_tickets, ticket_accepted, tickets_removed, poller_status, cookies_expired,
auto_relogin, booking_enriched, rules_updated, pause_expired`, dst — daftar lengkap dari
`hub.ts:6-20`). **Delta-sync `?since=` TIDAK dibangun di sini** — itu Fase 6.

## Struktur file

```
Backend/crates/poller/
  src/
    lib.rs
    state.rs          (PollerState per-akun: interval, pollCount, health, dedup handle refs)
    schedule.rs         (task loop: sleep+Notify select!, single-flight by construction)
    fetch.rs              (rotating window / full-sweep / fast-detect page fetch orchestration)
    hedge.rs                (hedged fetch — opt-in, default off)
    notif_watch.rs            (staggered lanes, exponential backoff, poke)
    login.rs                   (tier 2/3 in-proc + tier 1 HTTP client ke auth-sidecar)
    watchdog.rs                 (durable-primary 60s recreate)
    antidrift.rs                 (FetchOutcome, resurrect_pending, expire_stale_bookings)

Backend/crates/notifier/
  src/
    lib.rs, waha.rs, push_vapid.rs, message.rs (pesan template — port persis format acuan)

Backend/crates/ws-hub/
  src/
    lib.rs, hub.rs (axum WS handler + local connection registry), bridge.rs (Redis pub/sub),
    events.rs (WsEvent enum)

Backend/bin/auth-sidecar/
  src/main.rs   (+handler tier-1 browser login — chromiumoxide, proses terpisah dari
                 reactor-core, HTTP internal contract yang poller::login.rs konsumsi)
```

## Di luar cakupan (Fase 6+)

REST route (`/live?since=`, manual accept endpoint yang Fase 4's `try_claim_manual` sudah
siapkan) — Fase 6 (api-gateway). OTP gate untuk arm auto-accept global — Fase 6. UI yang
mengkonsumsi ws-hub — Fase 7. Watchdog-eksternal consumer untuk `spx:poller_heartbeat` —
tidak dibangun sama sekali sampai ada kebutuhan nyata (YAGNI, sesuai realitas acuan yang
juga tidak pernah membangunnya).

## Definition of Done — Fase 5

1. Poller task loop: single-flight terbukti BY CONSTRUCTION (test yang membuktikan tidak
   mungkin dua `poll_once` untuk akun sama tumpang tindih — misalnya lewat tipe/arsitektur,
   bukan hanya "tidak pernah keliatan race di test run") + `poke` membangunkan task dari
   `sleep` lebih awal (test dengan `tokio::time::pause`/`advance` membuktikan waktu yang
   nyata terlewat, bukan menunggu interval penuh).
2. Fast-detect & hedged fetch: default MATI dibuktikan test (tanpa env var, perilaku sama
   seperti tanpa fitur ini), DAN saat diaktifkan lewat env var berperilaku sesuai acuan.
3. Full sweep tiap 3 siklus: test yang menjalankan N siklus, hitung berapa kali full-sweep
   vs window-biasa terjadi, cocok `pollCount % 3 == 0`.
4. Notif watcher: staggered lanes + backoff eksponensial (250ms floor, 5000ms cap, reset ke
   0 begitu sehat) dibuktikan test nyata (bukan cuma baca konstanta), poke memicu full
   sweep pada siklus berikutnya.
5. Auto-login: tier 2/3 diporting dan diuji (mock SPX server via wiremock, pola Fase 3);
   tier 1 diuji lewat kontrak HTTP ke `auth-sidecar` (mock server berperan sebagai
   sidecar); urutan coba 1→2→3 dan fallback saat sidecar unreachable dibuktikan test.
   `auth-sidecar`'s browser-automation handler sendiri: test yang membuktikan endpoint
   HTTP-nya menerima request dan mengembalikan cookies (detail chromiumoxide-nya
   sendiri — test lebih terbatas karena butuh browser nyata; minimal proven lewat unit
   test pada layer parsing/orchestration-nya, bukan end-to-end terhadap SPX asli).
6. Watchdog durable-primary: test yang mensimulasikan poller primary "hilang", assert
   watchdog membuatnya ulang dalam siklus 60s berikutnya (pakai `tokio::time::pause`).
7. Anti-drift: `resurrect_pending`/`expire_stale_bookings` HANYA bisa dipanggil dengan
   `FetchOutcome` (bukan `HashSet` mentah) — test yang membuktikan sweep parsial
   (`fetch_complete=false`) TIDAK memicu expire (baik lewat tipe yang menolak compile,
   atau assertion eksplisit kalau tipe tidak bisa menutup ini 100%).
8. `notifier`: WAHA + VAPID push terkirim (mock server), fire-and-forget dibuktikan
   (kegagalan notifier tidak mem-propagate error ke caller — test yang sengaja bikin WAHA
   gagal dan assert caller tetap sukses).
9. `ws-hub`: channel per-sesi+per-akun, ping 30s, Redis pub/sub bridge (dua proses/koneksi
   `ws-hub` terpisah yang publish-subscribe silang lewat Redis nyata, membuktikan bridge
   benar-benar menjembatani, bukan cuma broadcast lokal).
10. `cargo test`/`clippy`/`deny` bersih workspace-wide, tidak ada dependency I/O yang tidak
    diharapkan (poller tidak depend chromiumoxide; ws-hub/notifier tidak depend sqlx
    langsung kecuali lewat store, dst — pola yang sudah konsisten sejak Fase 4).
