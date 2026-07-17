# Fase 7b — Nav shell + /command (design)

> Sub-fase kedua dari Fase 7, lanjutan langsung dari Fase 7a (login + fondasi desain token,
> `Docs/superpowers/specs/2026-07-17-fase-7a-login-design-foundation-design.md`). Sub-fase ketiga
> (7c: `/tickets` versi manajemen penuh, reuse komponen dari sini) dan seterusnya (7d: `/rules`,
> 7e: `/price`+`/settings`+`/activity`) — pembagian ini tetap indikatif.

## Kenapa dipecah begini

Nav shell + `/command` sekaligus mencakup Latency Tape (komponen canvas baru) + Live Ticket
Ticker dengan aksi accept optimistic + koneksi WS pertama di frontend — kompleksitasnya sebanding
dengan seluruh Fase 7a. Dipilih sebagai satu sub-fase (bukan dipecah lebih kecil lagi) karena nav
shell nyaris tak berguna diuji sendirian tanpa satu halaman nyata yang mengisinya, dan
`/command` adalah surface default (landing) — pasangan alami.

## Riset & temuan

- **Aplikasi referensi TIDAK punya struktur 7-surface TOWER.** Hanya 4 halaman
  (`/bookings` — 2143 baris, mencampur ticker+rules+latency+aksi accept jadi satu — `/login`,
  `/price`, `/settings`). Master spec TOWER sengaja memecah ini jadi arsitektur informasi baru;
  desain di bawah bukan porting, tapi requirement FUNGSIONAL (data apa yang perlu ditampilkan,
  aksi apa yang perlu ada) tetap diambil dari perilaku referensi.
- **Gap kritis: `local_dispatch_us` (metrik headline "≤1ms decision-path") belum pernah
  diinstrumentasi di backend, dari Fase 0 sampai Fase 6e.** Hanya `accept_e2e_ms` yang ada
  (`poller::dispatch::dispatch_booking`, diukur dari `Instant::now()` tepat SEBELUM panggilan
  HTTP `accept_booking` ke SPX — lihat `Backend/crates/poller/src/dispatch.rs` sekitar baris 102).
  Master spec Aturan Keras #4 mewajibkan DUA metrik terpisah. **Disepakati sesi ini: instrumentasi
  `local_dispatch_us` masuk scope 7b** (lihat bagian "Perubahan backend" di bawah) — bukan
  ditunda ke Fase 8, supaya Latency Tape sejak awal menampilkan metrik yang benar-benar jadi
  klaim utama proyek, bukan proxy yang salah kaprah.
- **Data real-time sudah tersedia lebih lengkap dari dugaan awal.** `Backend/crates/ws-hub/src/events.rs`'s `WsEvent` enum (porting 1:1 dari referensi) sudah punya varian
  `NewTickets`, `TicketAccepted`, `TicketRejected`, `TicketsRemoved`, `StatsUpdate`,
  `PollerStatus`, `CookiesExpired`, `AutoRelogin`, `RulesUpdated` — semua siap pakai, tidak perlu
  event baru untuk fungsi dasar. `GET /bookings/live` (Fase 6c) sudah ada untuk fetch awal
  sebelum WS mulai mengalir. `POST /bookings/:id/accept` (Fase 6c) sudah ada untuk aksi Terima.
- **WS upgrade browser-side sekarang genuinely bisa jalan** berkat fix Task 4 Fase 7a
  (`ws-hub::ws_handler_with_auth` menerima token dari cookie `HttpOnly`) — `/command` adalah
  KONSUMEN PERTAMA fix itu di frontend nyata. `new WebSocket('/ws')` dari browser otomatis
  mengirim cookie sesi tanpa perlu membangun `?session=` secara manual.
- **`payload di WsEvent::TicketAccepted`/`StatsUpdate`/`PollerStatus` berbentuk `serde_json::Value`
  (JSON bebas, mengikuti bentuk protokol referensi)** — field `localDispatchUs` baru ditambahkan
  ke payload yang sudah dikirim `pub_.publish_ticket_accepted(...)` (poller/dispatch.rs), pola
  yang sama seperti `latencyMs` yang sudah ada, bukan event/tipe baru.

## Palet, tipografi, tema

Tidak berubah dari Fase 7a — token `--color-*`/`--font-*` di `app.css` dipakai apa adanya. Warna
semantik untuk sub-fase ini: **teal (`--color-live`) untuk Latency Tape & indikator "hidup"**,
**amber (`--color-accent`) untuk aksi (tombol Terima) & spike/peringatan** — konsisten dengan
keputusan Fase 7a ("amber = aksi/perhatian, teal = data/status").

## Layout nav shell (divalidasi lewat companion visual)

**Dipilih: top bar horizontal saja, tanpa sidebar.** Logo+nama kiri, 7 tab nav di tengah
(`/command` aktif secara default), Health Pill + ikon Notification Center di kanan. Alasan
pengguna: lebih ringkas vertikal, konten dapat ruang lebih banyak — trade-off (7 label mulai
padat di layar sempit) diterima, ditangani dengan overflow-scroll horizontal pada breakpoint
mobile (bukan hamburger menu — tetap sesuai gaya "semua tab terlihat" yang dipilih).

Struktur route: `Frontend/src/routes/(app)/+layout.svelte` — route group baru, terpisah dari
`/login` yang TIDAK memakai nav shell ini. Health check sesi (redirect ke `/login` kalau tidak
ada sesi valid) dilakukan di level route group ini via `+layout.server.ts` (cek cookie sesi
lewat panggilan server-side ke `GET /auth/me`, bukan asumsi dari client).

## Latency Tape (divalidasi lewat companion visual)

**Dipilih: "Scope Trace"** — garis kontinu bergerak scroll kanan-ke-kiri di atas canvas gelap,
efek glow teal (persistensi fosfor via alpha-fade pada garis lama, bukan clear-canvas polos),
titik yang melebihi budget 1ms ditandai amber dengan glow terpisah. Angka p99 besar
(`IBM Plex Mono`) di bawah trace. **Buffer data**: array circular di memori klien (mis. 200 titik
terakhir), diisi dari field `localDispatchUs` di event `ticket_accepted`. TIDAK ada riwayat
tersimpan di DB — murni live, konsisten dengan sifat "osiloskop".

## Live Ticket Ticker (divalidasi lewat companion visual)

**Dipilih: baris kompak** — 1 baris = status dot + ID booking (mono) + rute + hasil
(latency/pending/diambil-lain). Newest-first, delta-merge dari event WS (`new_tickets` prepend,
`ticket_accepted`/`ticket_rejected` update baris yang sudah ada by ID, `tickets_removed` hapus by
ID). Badge "+N baru" muncul di atas list kalau user sedang scroll ke bawah saat event baru masuk
(scroll position bukan di top) — klik badge scroll ke atas + clear badge.

**Aksi Terima (disepakati sesi ini, masuk scope 7b):** baris dengan status `pending` punya tombol
"Terima" kecil. Klik → optimistic update (baris berubah ke state "Memproses..." dengan spinner,
tombol disabled) → `POST /bookings/:id/accept` (real, sudah ada) → sukses: baris dikonfirmasi
diterima (event WS `ticket_accepted` yang menyusul jadi sumber kebenaran akhir, memungkinkan
update dari operator lain/device lain juga tercermin) → gagal (409/500/network): baris kembali ke
`pending`, tombol aktif lagi, toast error singkat muncul (pesan generik dari
`ManualAcceptResponse.message`, bukan detail internal).

## Health Pill + Notification Center

**Health Pill** (glyph+teks, bukan warna saja — WCAG, sudah jadi requirement master spec): 3
state — `● LIVE` (teal, terhubung), `◐ RECONNECTING` (amber, mencoba sambung ulang), `○ TERPUTUS`
(danger, gagal >10 detik) — diisi dari state internal `$lib/ws.svelte.ts`, bukan dari event
`poller_status` (yang menggambarkan status akun SPX di poller, konsep berbeda dari status koneksi
WS klien sendiri — kedua hal ini digabung salah kalau dipakai bergantian; 7b HANYA membangun
health pill KONEKSI KLIEN, health per-akun poller dari `poller_status` dicatat sebagai potensi
komponen terpisah untuk sub-fase nanti, TIDAK dibangun sekarang — YAGNI, belum ada requirement
UI eksplisit untuk itu di luar apa yang divalidasi sesi ini).

**Notification Center**: ikon lonceng di top bar, sub-fase ini HANYA bangun ikon + panel kosong
("Belum ada notifikasi") — isi sungguhan (daftar notifikasi nyata, badge unread count) butuh
desain terpisah dan data source yang belum diklarifikasi (kemungkinan dari `notifier` crate atau
tabel `notifications` Fase 2 yang belum pernah dipakai production code manapun sejauh ini —
dicatat sebagai gap untuk sub-fase Notification Center nanti, bukan diam-diam dianggap selesai).

## Perubahan backend (scope 7b)

`Backend/crates/poller/src/dispatch.rs`: tambah `Instant::now()` baru di awal `dispatch_booking`
(sebelum rule-matching/claim), hitung durasi sampai TEPAT SEBELUM `let started = Instant::now()`
yang sudah ada (titik mulai pengukuran `accept_e2e_ms`) — dua metrik ini jadi dua segmen
berurutan dari timeline yang sama, tidak overlap. Hasil (`local_dispatch_us: u64`, mikrodetik)
ditambahkan sebagai field baru ke JSON payload `publish_ticket_accepted` yang sudah ada di
`finalize_win` (poller/dispatch.rs) — field baru murni tambahan, tidak mengubah struktur yang
sudah ada.

## Struktur file (indikatif — plan resmi akan memverifikasi ulang terhadap kode nyata)

```
Frontend/src/
  routes/
    (app)/
      +layout.svelte          # BARU: nav shell (top bar)
      +layout.server.ts       # BARU: cek sesi via GET /auth/me, redirect /login kalau invalid
      command/
        +page.svelte           # BARU: /command — Latency Tape + Ticket Ticker
  lib/
    ws.svelte.ts                # BARU: store koneksi WS (runes), reconnect+backoff
    ticker.ts                    # BARU: logic delta-merge murni (diuji terpisah dari komponen)
    components/
      TopNav.svelte              # BARU
      HealthPill.svelte           # BARU
      NotificationCenter.svelte    # BARU (shell/panel kosong)
      LatencyTape.svelte            # BARU (canvas)
      TicketTicker.svelte            # BARU
Backend/crates/poller/src/dispatch.rs  # DIUBAH: instrumentasi local_dispatch_us
```

## Di luar cakupan (sub-fase berikutnya)

- `/tickets` (versi manajemen penuh, filter/histori) — 7c.
- Isi nyata Notification Center (daftar notifikasi, unread count, sumber data) — belum
  diklarifikasi, sub-fase terpisah.
- Health pill per-akun poller (dari event `poller_status`) — dicatat, tidak dibangun sekarang.
- `/rules`, `/price`, `/settings`, `/activity` — tab nav sudah ada linknya (bagian shell), isi
  halaman masing-masing belum dibangun (404 sementara, sama seperti `/command` sendiri sampai
  sub-fase ini selesai).

## Definition of Done — Fase 7b

1. Nav shell tampil di semua route dalam grup `(app)`, 7 tab dengan `/command` default aktif,
   redirect ke `/login` kalau sesi tidak valid (dites: akses `/command` tanpa cookie sesi harus
   redirect, bukan menampilkan halaman kosong/error).
2. `local_dispatch_us` genuinely terukur dan mengalir lewat WS — dites dengan accept sungguhan
   (bukan hanya baca kode), nilai yang muncul di frontend masuk akal (sub-millisecond hingga
   beberapa ms, bukan nol atau angka yang jelas salah).
3. Latency Tape merender live dari data WS asli, berhenti bertambah (bukan menampilkan data
   palsu) saat WS terputus, animasi nonaktif saat `prefers-reduced-motion`.
4. Ticket Ticker: delta-merge (`new_tickets`/`ticket_accepted`/`ticket_rejected`/`tickets_removed`)
   diuji sebagai fungsi murni di `$lib`, badge "+N baru" bekerja saat scroll bukan di top.
5. Aksi Terima: optimistic update + `POST /bookings/:id/accept` sungguhan + revert pada gagal,
   dites end-to-end (Playwright) dengan skenario sukses DAN gagal.
6. Health Pill 3 state (LIVE/RECONNECTING/TERPUTUS) genuinely berubah saat koneksi WS
   diputus-sambung paksa dalam test, glyph berbeda per state (bukan cuma warna).
7. Keyboard-only walkthrough: navigasi antar tab, klik tombol Terima, semua bisa tanpa mouse.
8. `cargo test`/`clippy`/`deny` bersih workspace-wide (perubahan `dispatch.rs` menyentuh hot path
   yang sudah di-review ketat sebelumnya — regresi di sini serius). `pnpm check`/`pnpm build`
   bersih.
