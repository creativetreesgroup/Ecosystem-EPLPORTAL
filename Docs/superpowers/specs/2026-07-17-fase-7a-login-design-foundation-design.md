# Fase 7a — Login page + minimal design foundation (design)

> Sub-fase pertama dari Fase 7 (Web "TOWER" Command Center). Master spec Fase 7 mengharapkan
> "design PRD" sebagai acuan — dokumen itu **tidak ada** di repo ini (dicek eksplisit sesi ini,
> tidak ditemukan di mana pun), jadi desain di bawah ini dibangun dari nol lewat brainstorming
> dengan pengguna, bukan porting dari PRD yang sudah ada. Palet/tipografi yang divalidasi di sini
> jadi acuan untuk seluruh Fase 7 berikutnya (7b: `/command`+`/tickets`, 7c: `/rules`, 7d:
> `/price`+`/settings`+`/activity`, 7e: Notification Center + polish realtime — pembagian ini
> indikatif, boleh berubah saat masing-masing giliran dibrainstorm).

## Kenapa dipecah begini

Fase 7 mencakup 7 surface + 5 komponen signature sekaligus — terlalu besar untuk satu
spec/plan/implementasi. Dipecah per sub-fase, masing-masing lewat siklus penuh
spec→plan→implementasi→review sebelum lanjut, mengikuti disiplin yang sudah terbukti di Fase 6
(6a-6e). Sub-fase ini (7a) dipilih sebagai titik mulai karena dua alasan:

1. **Halaman paling sederhana secara UI** — aman untuk memvalidasi token desain pertama kali
   tanpa kompleksitas komponen realtime (Latency Tape, Live Ticket Ticker) yang belum ada
   acuannya.
2. **Satu-satunya surface yang genuinely butuh sesi login+cookie bekerja penuh** — kesempatan
   alami untuk menutup gap WS-auth-cookie (lihat di bawah) sebelum surface realtime pertama
   (`/command`, 7b) membutuhkannya.

## Cakupan token desain: minimal, bukan fondasi penuh

Disepakati eksplisit dengan pengguna: sub-fase ini membangun token Tailwind v4 yang **genuinely
dibutuhkan halaman login** (palet, tipografi, mekanisme dark/light), BUKAN seluruh design system
(nav shell 7 surface, komponen bersama lain). Token yang dibangun di sini **reusable**, tidak
dibuang saat sub-fase berikutnya dikerjakan — tapi nav shell/layout bersama untuk surface
berautentikasi (sidebar, header, dst.) sengaja belum dibangun di sini karena belum ada surface
kedua yang membutuhkannya.

## Riset & temuan (sebelum desain divalidasi)

- **Referensi (`creativetrees/Ecosystem-PortalSPX`) juga SvelteKit** — `apps/web/src/routes/login/+page.svelte` dibaca penuh. Alur fungsionalnya LEBIH KOMPLEKS dari yang TOWER butuhkan hari ini: mengirim device fingerprint, dan API login-nya mengembalikan state `active`/`pending`/`needs_spx_setup` yang men-drive orkestrasi auto-login SPX headless (Playwright) langsung dari halaman login. **TOWER TIDAK mereplikasi ini** — `POST /auth/portal-login` TOWER (`Backend/crates/api-gateway/src/routes/auth.rs`) sudah final sejak Fase 6a: request `{username, password}` saja, response `{username, display_name, is_main_account}` + `Set-Cookie` sesi, tanpa fingerprint, tanpa state bercabang. Konektivitas SPX (`POST /auth/spx-login/:label`) sudah menjadi endpoint terpisah, bukan bagian alur login portal. Halaman ini secara fungsional LEBIH SEDERHANA dari acuan — sengaja, bukan kelalaian.
- **`docker/Caddyfile` (edge proxy lokal) rusak untuk SEMUA rute backend hari ini.** Rule satu-satunya (`handle_path /api/* { reverse_proxy tower-reactor-core:8081 }`) tidak cocok dengan satu pun prefix rute yang benar-benar ada di `api-gateway` (`/auth`, `/bookings`, `/prices`, `/branding`, `/locations`, `/bot`, `/q`, `/accept`, `/ws`, `/healthz` — semua TANPA prefix `/api`). Ini baru ketahuan sekarang karena belum ada frontend yang benar-benar memanggil backend lewat edge sampai sub-fase ini. Master spec Fase 8 sendiri mengonfirmasi skema routing yang dimaksud sejak awal: "label routing /api /auth /ws -> reactor-core, else web" (untuk Traefik VPS overlay) — beberapa prefix eksplisit, bukan satu `/api/*` generik — dan menjanjikan "**Nol perubahan kode**" saat Caddy diganti Traefik, artinya skema routing ini harus SUDAH BENAR sebelum Fase 8, bukan diperbaiki di sana. **Masuk cakupan sub-fase ini** (perbaikan konfigurasi, bukan keputusan desain, jadi diputuskan langsung tanpa brainstorming lebih lanjut).
- **Gap WS-auth-cookie (tracked note Fase 6a, `Docs/superpowers/specs/2026-07-15-fase-6-api-gateway-design.md`)**: `ws_handler_with_auth` hanya menerima token lewat `?session=` query param; `portal_login` sengaja menaruh token di cookie `HttpOnly` yang TIDAK BISA dibaca JavaScript (pilihan keamanan yang benar dengan sendirinya). Browser nyata tidak punya cara membangun `ws://…/ws?session=<token>`. Tracked note menyebut dua kandidat resolusi; **disepakati dengan pengguna sesi ini: opsi (a)** — WS upgrade handler JUGA menerima token dari cookie `HttpOnly` (selain `?session=` yang tetap ada untuk kompatibilitas/test), bukan opsi (b) (WS-ticket sekali pakai terpisah) karena lebih sederhana dan menggunakan ulang sesi yang sudah ada tanpa mekanisme baru. **Masuk cakupan sub-fase ini** meskipun `/command` (surface pertama yang benar-benar memakai WS) belum dibangun — supaya begitu 7b dikerjakan, WS live sudah pasti jalan.
- **`Frontend/` sudah scaffold Tailwind v4** (Fase 0): `src/app.css` berisi `@import 'tailwindcss'; @theme { /* Fase 7 fills this in */ }` — placeholder yang persis pas untuk sub-fase ini. SvelteKit 5.56 (runes), `adapter-node`, `@tailwindcss/vite` sudah terpasang. Tidak ada dependency baru yang perlu ditambah untuk styling.
- **`GET /branding` sudah ada** (Fase 6d, publik, tanpa sesi) — mengembalikan `title`, `subtitle`, `site_name`, `brand_tag`, `logo_data_uri`, `favicon_data_uri`. Halaman login memakai ini untuk identitas visual (nama situs, logo), bukan hardcode — mencerminkan pola acuan yang juga menarik branding dari DB.

## Palet & tipografi (divalidasi lewat companion visual, 2026-07-17)

Tiga arah palet dipresentasikan (masing-masing diterapkan ke preview nyata: angka latency,
tombol aksi, indikator status — bukan swatch lepas). **Dipilih: "Balanced Duo".**

- **Graphite:** dark `#15181c` (sedikit lebih hangat dari netral murni) / light `#f7f6f3`.
- **Amber:** untuk AKSI dan PERINGATAN (tombol utama, status "perlu perhatian"). Dark
  `#eab308`, light **dipertajam** ke `#b45309` — versi dark TIDAK dipakai langsung di light mode
  karena gagal rasio kontras AA (4.5:1) di atas latar terang; ini penyesuaian sengaja, bukan
  sekadar dibalik.
- **Teal:** untuk DATA dan STATUS HIDUP/TERHUBUNG (angka latency, indikator koneksi). Dark
  `#2dd4bf`, light dipertajam ke `#0d9488` dengan alasan kontras yang sama.
- **Koreksi (Task 1 review finding, dilacak di sini): nilai light-mode teal/danger di atas belum
  cukup tajam.** `#0d9488` (teal) dan `#dc2626` (danger, token error/`--color-danger`, minimal
  addition di luar dua aksen di atas — lihat catatan plan) dipilih sebelum matematika kontras
  nyata dihitung untuk sub-fase ini; dihitung ulang saat implementasi Task 1 dengan formula
  WCAG relative-luminance (bukan dikira-kira mata) dan ternyata gagal/mepet gagal 4.5:1:
  `#0d9488` = 3.46:1 (gagal), `#dc2626` = 4.47:1 (mepet, tetap gagal) terhadap
  `--color-bg-base` (`#f7f6f3`). Dikoreksi ke `#0f766e` (teal, 5.07:1) dan `#b91c1c` (danger,
  5.99:1), diverifikasi dua kali (implementer + reviewer, angka sama persis). Lihat
  `Frontend/src/app.css`'s `[data-theme='light']` block dan `Frontend/src/lib/tokens.ts`.
- **Alasan pemilihan (dari pengguna):** dua aksen setara (bukan satu dominan) terasa lebih tenang
  untuk dipandangi berjam-jam (shift kerja) — amber = aksi/perhatian, teal = data/status, makna
  warna jadi konsisten dan bisa diprediksi di seluruh Fase 7, bukan cuma dekorasi.
- **Tipografi:** Space Grotesk (judul/heading, weight 700), IBM Plex Mono (angka/data — latency,
  ID booking, kode), Inter (teks badan/UI biasa). Font di-self-host (bukan Google Fonts CDN) —
  konsisten dengan "no hex mentah di komponen" dan kontrol penuh atas FOUT/FOIT.
- **Dark+light:** keduanya WAJIB (master spec), lewat atribut `data-theme` di elemen root,
  di-set oleh script inline di `app.html` SEBELUM CSS pertama dimuat (anti-flash — baca
  `localStorage` lalu `prefers-color-scheme` sebagai fallback).

## Layout halaman login (divalidasi lewat companion visual)

**Dipilih: "Centered Card"** — kartu login di tengah layar, latar polos (bukan split-hero dengan
panel cuplikan Latency Tape). Alasan pengguna: pola familiar, fokus penuh ke form, tidak perlu
data/placeholder tambahan di panel kiri yang belum ada isinya, dan tetap benar di layar sempit
tanpa perlu logic tumpuk-kolom tambahan.

Struktur kartu (mengikuti pola fungsional acuan, visual baru total):
- Logo/nama situs dari `GET /branding` di atas kartu (fallback ke placeholder default bila belum
  dikonfigurasi tenant).
- Header kartu kecil ("MASUK KE PORTAL").
- Banner error (muncul kondisional, `aria-live="polite"`) — pesan generik **"Username atau
  password salah"**, TIDAK membedakan user-tidak-ada vs password-salah (konsisten dengan
  proteksi enumeration-timing yang sudah ada di `portal_login` backend — UI tidak boleh
  membocorkan lewat pesan apa yang backend sengaja sembunyikan lewat timing).
- Field username (text, autocomplete `username`), field password (dengan toggle show/hide —
  pola standar, murah dibangun, membantu aksesibilitas terutama di mobile — diputuskan langsung
  tanpa brainstorming lebih lanjut karena bukan keputusan dengan trade-off berarti).
- Tombol submit: state default / loading (spinner, disabled) / disabled (form belum valid).
  Submit via klik ATAU Enter di salah satu field.
- **TIDAK ADA**: field fingerprint, checkbox "ingat saya", state bercabang
  active/pending/needs_spx_setup — semua ini bagian dari acuan yang tidak punya padanan di
  backend TOWER hari ini (YAGNI: jangan bangun UI untuk kapabilitas backend yang tidak ada).

## Alur data

1. Pengguna isi username+password, submit (klik/Enter).
2. `fetch('/auth/portal-login', { method: 'POST', credentials: 'include', body: {...} })` —
   path RELATIF (bukan absolute URL ke backend), memakai proxy edge yang sama dengan
   deployment sungguhan (lihat perbaikan Caddyfile di atas) — tidak perlu CORS di produksi.
   Untuk dev lokal tanpa Docker (`pnpm dev` langsung), `vite.config.ts` mendapat proxy
   `/auth`, `/bookings`, dst. -> `http://127.0.0.1:8081` (port dev `reactor-core`), pola yang
   sama seperti daftar prefix Caddyfile di atas — satu sumber kebenaran untuk daftar prefix,
   didokumentasikan di kedua tempat.
3. Sukses (200): cookie sesi (`HttpOnly`, `Secure` di prod) sudah otomatis ter-set oleh
   `Set-Cookie` response — SvelteKit tidak perlu (dan tidak boleh) menyentuh token secara
   eksplisit. Redirect client-side ke `/command`. `/command` belum dibangun (7b) — akan 404
   untuk sementara; ini sengaja (YAGNI: tidak bikin halaman placeholder palsu), didisclosekan
   di sini supaya tidak dikira lupa saat sub-fase ini selesai.
4. Gagal (401): tampilkan banner error generik, form tetap terisi (kecuali password, dikosongkan
   demi kebersihan — pola standar), fokus kembali ke field username.
5. Gagal jaringan (fetch reject): banner error "Tidak dapat menghubungi server. Coba lagi."

## Aksesibilitas (WCAG 2.2 AA — wajib, non-negotiable per instruksi pengguna)

- Label eksplisit (`<label for=...>`) tiap field, bukan cuma placeholder.
- Focus ring 2px, kontras tinggi, terlihat di dark DAN light mode.
- Target sentuh tombol/toggle ≥24×24px.
- Banner error: `role="alert"` + `aria-live="polite"` supaya pembaca layar mengumumkannya tanpa
  perlu fokus pindah paksa.
- Toggle show/hide password: `aria-pressed`, label yang berubah sesuai state ("Tampilkan
  password" / "Sembunyikan password"), bukan cuma ikon.
- Kontras warna: sudah diverifikasi di companion visual (amber/teal dipertajam khusus untuk
  light mode, lihat di atas) — perlu re-verifikasi dengan alat kontras nyata (bukan cuma mata)
  saat implementasi, dicatat sebagai langkah eksplisit di plan.
- `prefers-reduced-motion`: transisi/animasi (termasuk spinner loading) di-nonaktifkan atau
  diperlambat drastis bila preferensi ini aktif.
- Keyboard penuh: seluruh alur (isi form, toggle password, submit) bisa dilakukan tanpa mouse,
  urutan tab logis.

## Struktur file

```
Frontend/
  src/
    app.css              # DIISI: @theme dengan token warna/font/radius Balanced Duo
    app.html             # DIISI: script anti-flash data-theme
    lib/
      tokens.ts           # BARU: mirror token warna dalam TS (canvas/JS, no hex mentah)
      api.ts               # BARU: helper fetch tipis, path relatif, credentials:'include'
      theme.ts              # BARU: baca/tulis preferensi tema (localStorage + prefers-color-scheme)
    routes/
      login/
        +page.svelte        # BARU: halaman login
        +page.server.ts     # BARU (bila perlu): redirect-jika-sudah-login via cek cookie sesi
  vite.config.ts          # DIUBAH: proxy dev ke reactor-core untuk prefix yang sama dgn Caddyfile
docker/
  Caddyfile                 # DIUBAH: rule routing eksplisit per prefix backend nyata
Backend/
  crates/ws-hub/src/hub.rs   # DIUBAH: ws_handler_with_auth terima token dari cookie HttpOnly juga (bukan cuma ?session=)
```

## Di luar cakupan (sub-fase berikutnya / fase lain)

- Nav shell bersama, sidebar, header aplikasi untuk surface berautentikasi — sub-fase 7b+.
- 6 surface lain (`/command`, `/tickets`, `/rules`, `/price`, `/settings`, `/activity`) dan 5
  komponen signature (Latency Tape, Live Ticket Ticker, Rule Builder, Health Pills, Notification
  Center) — sub-fase 7b+.
- Short-code/HMAC quick-accept LINK GENERATION dari sisi notifier (tracked gap dari Fase 6e) —
  tidak relevan dengan login page, tetap tercatat di design doc Fase 6.
- Opsi (b) dari gap WS-auth-cookie (WS-ticket sekali pakai) — tidak dipilih, dicatat di sini
  sebagai alasan kenapa TIDAK dibangun, bukan diam-diam dilupakan.
- Redirect pasca-login ke halaman SELAIN `/command` (mis. deep-link balik ke halaman yang
  memicu redirect ke login) — YAGNI, tidak ada requirement untuk itu hari ini.

## Definition of Done — Fase 7a

1. `GET /login` menampilkan kartu sesuai layout yang divalidasi, branding dari `GET /branding`,
   dark+light bekerja tanpa flash, kontras AA lolos di kedua tema (diverifikasi dengan alat
   kontras nyata, bukan cuma mata).
2. Login sukses (kredensial valid) benar-benar men-set cookie sesi dan redirect ke `/command`
   (dites end-to-end lewat Playwright terhadap `reactor-core` sungguhan, bukan mock).
3. Login gagal (kredensial salah) menampilkan pesan generik yang sama untuk user-tidak-ada
   maupun password-salah — dites eksplisit kedua kasusnya menghasilkan pesan identik.
4. `docker compose up` end-to-end: request dari browser ke `tower-web` (login page) yang
   memanggil `/auth/portal-login` benar-benar tembus Caddy ke `reactor-core` dan kembali —
   ini bukti langsung bahwa perbaikan Caddyfile bekerja, bukan cuma dibaca ulang.
   **Status: ditutup lewat verifikasi Task 2 sendiri** (real Caddy + real `tower-web` + stand-in
   sementara untuk `tower-reactor-core` — lihat tracked note di bawah untuk alasannya), BUKAN
   lewat Task 6 (Playwright Task 6 jalan terhadap `pnpm dev` + `cargo run` native, tidak
   menyentuh Docker/Caddy sama sekali) — jangan berasumsi Task 6 menutup item ini juga.
5. WS upgrade (`/ws`) menerima token dari cookie `HttpOnly` — dites dengan request WS nyata yang
   TIDAK menyertakan `?session=` sama sekali, hanya cookie, dan berhasil ter-autentikasi.
6. Keyboard-only walkthrough penuh (isi form, toggle password, submit, baca error) tanpa mouse.
7. `cargo test`/`clippy`/`deny` tetap bersih workspace-wide (perubahan `ws.rs` menyentuh kode
   Rust yang sudah di-review sebelumnya).
8. `pnpm check` (svelte-check) bersih; build produksi (`pnpm build`) sukses.

## Tracked note: `reactor-core`/`auth-sidecar` Docker image tidak bisa di-build (Task 2 whole-branch review finding, ditemukan sesi ini — disclosed, non-blocking untuk Fase 7a, tapi nyata)

`Docker/reactor-core.Dockerfile` (dan kemungkinan besar `auth-sidecar.Dockerfile`, base builder yang sama) **tidak bisa membangun image `reactor-core` sungguhan** di lingkungan ini — builder stage-nya (`rust:1-slim-bookworm` + `cmake` saja) kekurangan toolchain yang genuinely dibutuhkan `spx-client`'s `wreq`→`btls-sys` dependency untuk vendor BoringSSL: `git` (bookkeeping vendoring), lalu `make`/`gcc`/`g++` (cmake butuh generator), lalu `libclang` (untuk `bindgen`). Dikonfirmasi langsung sesi ini (Task 2, dengan mem-patch sementara lalu me-revert penuh — tidak masuk commit). Bukti independen tambahan: image `tower-tower-reactor-core:latest` yang ter-cache di environment ini dibuild 2026-07-12/13, SEBELUM `api-gateway` benar-benar di-mount ke `reactor-core` (commit `4c820ec`, 2026-07-15) — artinya tidak pernah ada build sukses dari kode `reactor-core` yang sekarang.

Ini gap infrastruktur nyata yang baru ketahuan karena baru sekarang (Task 2, verifikasi Caddyfile) ada yang benar-benar mencoba `docker compose up` penuh sejak Fase 0. **Ditutup untuk sub-fase ini** lewat strategi verifikasi Task 2 (stand-in sementara untuk proses `reactor-core`, real Caddy + real network + real `tower-web`, dihapus setelah verifikasi — lihat ledger Task 2). Perbaikan toolchain Dockerfile-nya sendiri **dilacak untuk Fase 8** (sudah memegang hardening Docker/deployment secara umum, pola yang sama seperti gap provisioning `app_role`) — bukan pekerjaan sub-fase login page.
