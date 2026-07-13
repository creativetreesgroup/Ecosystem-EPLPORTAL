# Fase 3 — spx-client + security kripto (design)

Bagian dari [TOWER master spec](../../tower-master-spec.md). Sumber acuan fungsional:
`/tmp/spx-portal-ref` (clone lokal, sementara, dari `creativetrees/Ecosystem-PortalSPX` —
lihat catatan di master spec). File acuan persis yang dipakai untuk desain ini (hasil riset
mendalam, bukan tebakan):

- `apps/api/src/services/spx.ts` (1052 baris) — HTTP client SPX, `normalizeBooking`,
  `classifyAcceptResponse`.
- `apps/api/src/services/spx-auth.ts` — auto-login, penyimpanan kredensial agency terenkripsi.
- `apps/api/src/lib/quicktoken.ts` — HMAC quick-accept token.
- `apps/api/src/lib/session.ts` — sesi opaque server-side.
- `apps/api/src/routes/auth.ts` — password hashing (bcrypt), cookie sesi.
- `apps/api/src/services/webhook.ts` — penyimpanan WAHA API key (plaintext, bug yang mesti
  ditutup).
- `apps/api/src/services/spx-accept.test.ts` — 8 kasus pesan retcode SPX asli (satu-satunya
  "korpus fixture" nyata yang tersedia — lihat catatan di bawah).

## Tujuan

Dua deliverable dalam satu fase (sesuai judul master spec, "spx-client + security kripto"):

1. **`spx-client`**: klien HTTP SPX dengan TLS-impersonation, `SpxBooking` penuh +
   `normalizeBooking`, klasifikasi retcode accept (`taken/auth/transient/agency_dup`).
2. **Security kripto**: envelope encryption (master key -> HKDF -> AES-256-GCM),
   `SecretString`+zeroize, dan **menutup 3 gap acuan yang sudah dikonfirmasi nyata** (bukan
   dugaan — dibaca langsung dari kode acuan, lihat "3 Gap Keamanan" di bawah).

## Riset acuan — temuan kunci (ringkas; laporan lengkap ada di riwayat sesi)

### Bentuk `spx-client` acuan
- 10 endpoint terkonfirmasi: `bidding/list`, `count_v2`, `request/list`, `accept`,
  notification-count, `log/list` (GET), `user/list`, `profile` (multi-fallback), plus 2
  endpoint tambahan yang dipakai acuan (`booking_overview` fallback, `booking_log` probe) yang
  ikut di-port karena `fetchBookings`/`fetchBookingLog` bergantung padanya.
- Auth murni **cookie-based** (bukan bearer token) — `Cookie` header dari `SpxCookies`
  (10 field: `fms_user_skey, fms_user_id, fms_user_agency_id, csrftoken, spx_uk, spx_cid,
  spx_uid, spx_agid, spx_st, ds, spx-admin-device-id`), plus header `X-CSRFToken` dan
  `device-id` eksplisit untuk endpoint line-haul bidding.
- `SpxBooking` acuan (`spx.ts:7-37`) punya **29 field**, jauh lebih besar dari subset 11-field
  yang Fase 1 pakai untuk matcher. `normalizeBooking` (`spx.ts:116-195`) melakukan ekstraksi
  multi-key defensif (`pick(obj, ...keys)` — ambil key pertama yang non-empty) dan beberapa
  aturan non-trivial: `vehicleType` string angka-murni dibuang (dianggap kode, bukan label
  asli); `status` numerik di-map ke 3 nilai; `bookingType` diklasifikasi dari `booking_name`
  ASLI (bukan fallback `bookingId`) via `bookingTypeOf` (Fase 1's `booking_type_of`).
- `classifyAcceptResponse` (`spx.ts:922-944`) adalah fungsi murni 6-kategori (`ok, agency_dup,
  taken, transient, auth, error`) berbasis regex pada pesan (lowercased) — **urutan cek
  penting**: `agency_dup` dicek SEBELUM pola idempotent-`ok`, supaya "agency lain sudah ambil"
  tidak salah diklasifikasi sebagai sukses sendiri.
- **Tidak ada TLS/JA3 fingerprint impersonation nyata di acuan** — hanya spoofing header statis
  (User-Agent Chrome 148 + client-hints `sec-ch-ua`). Requirement rquest TLS-impersonation di
  master spec adalah kapabilitas BARU, bukan port dari sesuatu yang sudah ada.
- **Tidak ada file fixture JSON body SPX asli** di repo acuan sama sekali (dicek eksplisit,
  nihil). Test acuan pakai objek sintetis buatan tangan. Satu-satunya string pesan SPX ASLI
  yang terdokumentasi adalah 8 kasus di `spx-accept.test.ts` (dipakai untuk
  `classifyAcceptResponse`). **Requirement master spec "korpus fixture body SPX asli sebagai
  test" tidak bisa dipenuhi secara literal** — tidak ada yang bisa di-porting karena tidak
  pernah direkam di acuan. Ditutup dengan: (a) 8 pesan retcode asli itu tetap dipakai verbatim
  untuk test `classify_accept_response`, (b) fixture untuk `normalize_booking` dibangun dari
  nama-key yang terdokumentasi di `normalizeBooking`'s multi-key fallback logic (bukan dari
  body yang direkam) — didokumentasikan sebagai keterbatasan yang jujur, bukan disembunyikan.

### 3 Gap Keamanan (dikonfirmasi via baca kode, bukan dugaan)

1. **`SESSION_SECRET` di-pad jadi AES key** (`spx-auth.ts:16-18`):
   `Buffer.from(env.SESSION_SECRET.padEnd(32,'0').slice(0,32), 'utf-8')` — dipakai langsung
   sebagai key AES-256-GCM. Kalau secret < 32 karakter, di-pad pakai ASCII `'0'` (entropi
   turun drastis); kalau > 32 karakter, di-truncate diam-diam. Tidak ada KDF sama sekali.
2. **Satu secret dipakai untuk AES DAN HMAC** — `SESSION_SECRET` yang sama memberi makan (a)
   key AES-256-GCM di atas, DAN (b) `createHmac('sha256', SESSION_SECRET)` di
   `quicktoken.ts:7,10` untuk menandatangani token quick-accept (yang outputnya sendiri
   di-truncate ke 24 karakter base64url = 144 bit). `.env.example:18` mengonfirmasi ini
   disengaja: *"Also derives the AES-256-GCM key for stored SPX credentials."*
3. **WAHA API key plaintext** — `BotSettings.wahaApiKey` disimpan sebagai string mentah di
   Redis (`webhook.ts:464-466`, key `spx:bot:settings`, `JSON.stringify(s)` tanpa enkripsi
   apa pun). Satu-satunya "proteksi" adalah masking saat ditampilkan ke UI (`bot.ts:48`), bukan
   proteksi penyimpanan.

Temuan tambahan yang relevan untuk desain (bukan gap, tapi menentukan bentuk migrasi/kode):
- Kredensial SPX acuan disimpan di **Redis global** (satu key untuk semua, bukan per-tenant/
  per-agency), username **plaintext**, hanya password yang dienkripsi, **tidak ada
  `key_version`** (tidak ada dukungan rotasi kunci). Fase 2's tabel `agency_credentials` sudah
  disiapkan lebih baik: per-tenant, kolom `ciphertext bytea, nonce bytea, key_version int`
  eksplisit — desain Fase 3 tinggal isi kolom itu dengan benar, bukan reinvent.
- Password user/portal acuan pakai **bcrypt** (`Bun.password.hash(password,'bcrypt')`), BUKAN
  plaintext dan BUKAN argon2. Master spec eksplisit minta **argon2id** — ini upgrade yang
  disengaja dari acuan, bukan koreksi bug.
- Sesi acuan SUDAH opaque token server-side (`randomBytes(32).toString('hex')`, disimpan di
  Redis+Postgres, cookie httpOnly), BUKAN JWT — sudah sesuai master spec, tidak perlu migrasi
  arsitektur, hanya perlu di-port ke Rust dengan hashing token yang benar (Fase 2's
  `portal_sessions.token_hash` sudah mengasumsikan token di-hash, bukan disimpan mentah — lihat
  di bawah).

## Keputusan arsitektur

### Kripto tinggal di `spx-client::crypto`, BUKAN crate baru

Master spec's bagian ARSITEKTUR mendaftar persis 8 crate + 2 bin dan bilang "ikuti persis".
Menambah crate ke-9 (`crypto`) akan melanggar itu secara literal. Judul fase ini sendiri —
"spx-client + security kripto" — mengelompokkan keduanya sebagai satu deliverable, jadi
primitif kripto (envelope encryption, `SecretString`, argon2id, token sesi) hidup sebagai
modul publik `spx_client::crypto::{secret, envelope, password, session_token}`, bukan
workspace member terpisah.

**Trade-off yang disadari dan diterima**: Fase 6 (`api-gateway`) nanti butuh
`password`/`session_token` untuk login — itu berarti `api-gateway` akan depend ke `spx-client`
sebagai path-dependency, ikut menarik `rquest`+TLS-impersonation deps yang sebenarnya tidak
dipakai `api-gateway` secara langsung (compile-time lebih berat, bukan masalah korektnes).
Alternatif (crate `crypto` terpisah) lebih bersih secara dependency graph tapi melanggar
"ikuti persis" secara harfiah. Keputusan: ikuti arsitektur yang didikte, terima biaya compile
sebagai trade-off yang wajar — bisa direvisit di Fase 6 kalau biayanya ternyata signifikan.

### Docker secret file — perlu wiring Compose baru, bukan cuma kode Rust

`Docker/docker-compose.yml` belum punya stanza `secrets:` (dicek — nihil). Master key untuk
envelope encryption butuh file 0400 yang di-mount ke `reactor-core`/`auth-sidecar` via Compose
`secrets:` top-level + per-service `secrets:` list (bukan env var — env var kebaca lewat
`docker inspect`/`/proc/<pid>/environ`, file secret dengan permission 0400 tidak). Fase 3 harus
menambah: (a) `Docker/docker-compose.yml` `secrets:` stanza menunjuk file lokal (mis.
`./secrets/tower_master_key`, di-gitignore, dibuat sekali via `openssl rand -out ... 32` oleh
dev/operator — bukan digenerate otomatis oleh kode, supaya operator sadar dan bisa backup),
(b) `Docker/.env.example`'s komentar soal "secrets management arrives Fase 3" diperbarui jadi
instruksi konkret, (c) `.gitignore` entry untuk direktori secrets lokal kalau belum ada. Loader
Rust-nya baca dari path file (`/run/secrets/tower_master_key` di dalam container per konvensi
Compose), bukan dari env var — env var hanya untuk path-nya sendiri kalau perlu dev-override.

### Envelope encryption

```
Docker secret file (0400, /run/secrets/tower_master_key, 32 byte random)
  -> load sekali saat startup, simpan sebagai SecretString/Secret<[u8;32]>
  -> HKDF-SHA256(master_key, info=<purpose-scoped label>) -> subkey 32-byte per purpose
  -> AES-256-GCM(subkey, nonce random 96-bit per enkripsi, AAD = purpose label + tenant_id)
  -> ciphertext (termasuk 16-byte auth tag di akhir, konvensi standar `aes-gcm` crate)
```

Purpose label per pemakaian (mencegah reuse key lintas keperluan — inilah struktural fix untuk
Gap #2):
- `"tower.agency-credential.v1"` — enkripsi username+password SPX (`agency_credentials`).
- `"tower.waha-key.v1"` — enkripsi WAHA API key.
- HMAC quick-accept token TIDAK pakai AES sama sekali — subkey HKDF terpisah
  (`"tower.quick-accept-hmac.v1"`) dipakai murni untuk HMAC-SHA256, bukan AES — secara
  struktural tidak mungkin reuse key AES==key HMAC karena keduanya derive dari `info` string
  yang berbeda lewat HKDF yang sama, tapi subkey yang keluar berbeda.

Nonce SELALU random per enkripsi (bukan counter/deterministic) — dibangkitkan via OS CSPRNG
(`rand::rngs::OsRng` atau `aes_gcm`'s built-in nonce generator). AAD mengikat ciphertext ke
konteksnya (purpose label, dan `tenant_id` untuk `agency_credentials`) supaya ciphertext dari
satu tenant/purpose tidak bisa "dipindah" secara diam-diam ke row lain dan tetap valid.

`key_version` (kolom yang sudah ada di `agency_credentials` sejak Fase 2) menyimpan versi
master key yang dipakai saat enkripsi — bukan versi HKDF `info` label. Fase 3 hanya
mengimplementasikan `key_version=1` (rotasi kunci penuh, multi-master-key, adalah scope Fase
8/operasional, bukan Fase 3 — didokumentasikan sebagai extension point, bukan dibangun
sekarang, YAGNI).

### `SecretString` + zeroize

Tipe wrapper generik (pakai crate `secrecy` + `zeroize`) untuk SEMUA nilai rahasia yang lewat
memori: master key, subkey hasil HKDF, password plaintext (sebelum di-hash), token sesi
plaintext (sebelum di-hash untuk disimpan), kredensial SPX plaintext (sebelum dienkripsi/
setelah didekripsi). `Debug`/`Display` di-redact (`[REDACTED]`), tidak pernah ter-log oleh
`tracing`. Ini penerapan langsung Aturan Keras #5 ("Tak ada secret plaintext di mana pun —
log, Redis, DB, .env prod. Tipe SecretString+zeroize wajib").

### WAHA key: tersimpan di `site_settings`, bukan tabel baru

Acuan simpan `BotSettings` sebagai satu blob Redis. Fase 2 tidak punya tabel khusus bot/WAHA
settings — daripada menambah migrasi baru untuk satu field terenkripsi, WAHA key (ciphertext +
nonce, base64-encoded) disimpan sebagai bagian dari `site_settings` yang sudah ada
(`tenant_id, key='waha_settings', value=jsonb {ciphertext_b64, nonce_b64, key_version}`).
`site_settings.value jsonb` memang didesain generik untuk kasus seperti ini di Fase 2. Field
WAHA lain yang non-sensitif (base URL WAHA instance, session name) tetap plaintext di jsonb
yang sama — hanya `wahaApiKey` yang dienkripsi.

### Password hashing: argon2id

Ganti bcrypt acuan dengan **argon2id** (crate `argon2`), sesuai permintaan eksplisit master
spec. Parameter default OWASP-recommended (m=19456 KiB, t=2, p=1) kecuali ada alasan kuat
untuk beda — didokumentasikan di kode, bukan angka ajaib tanpa penjelasan.

### Sesi opaque 256-bit

Token sesi = 256-bit random (`OsRng`, 32 byte) di-encode base64url untuk dikirim sebagai
cookie httpOnly. Yang disimpan ke `portal_sessions.token_hash` (Fase 2) adalah **SHA-256 hash**
dari token itu, BUKAN token mentah — pola "lookup by hash" ini artinya kalaupun DB ter-dump,
attacker tidak bisa menyamar jadi sesi manapun (harus punya token asli untuk hash-match).
Verifikasi sesi: hash token dari cookie yang masuk, `SELECT ... WHERE token_hash = $1`,
constant-time bukan concern tambahan karena lookup lewat index unik (bukan perbandingan
string manual). Token plaintext HANYA pernah ada di response Set-Cookie satu kali saat login,
tidak pernah disimpan lagi di mana pun setelah itu.

### `SpxBooking` -> `core_domain::Booking`

`spx-client` membangun `SpxBooking` penuh (29 field, mirror acuan persis) via
`normalize_booking(raw: &serde_json::Value) -> SpxBooking`, lalu fungsi terpisah
`to_core_booking(&SpxBooking) -> core_domain::Booking` memetakan turun ke subset 11-field yang
Fase 1 sudah bangun (tidak mengubah `core_domain::Booking` sama sekali — kontrak Fase 1 tetap
utuh, hanya dikonsumsi).

### TLS-impersonation: rquest, best-effort Chrome preset

> **[KOREKSI — ditemukan saat penulisan plan]** `rquest` sudah **tidak dipublikasikan lagi**
> (di-rename jadi **`wreq`**, Apache-2.0). Preset impersonation Chrome hidup di crate terpisah
> `wreq-util`, yang line stabilnya (2.x) ternyata **GPL-3.0** (tidak lolos `deny.toml`) —
> line Apache-2.0-nya hanya ada di pre-release `3.0.0-rc.x`. Plan
> (`Docs/superpowers/plans/2026-07-13-fase-3-spx-client-crypto.md`, Task 8) memberi implementer
> 2 opsi eksplisit: pin `wreq-util` pre-release Apache-2.0 (JA3 preset nyata, direkomendasikan)
> atau `wreq` saja tanpa preset (fallback license-clean, setara acuan yang cuma spoof header).
> Preset Chrome tertinggi yang tersedia saat riset: **Chrome137** (bukan 148 seperti target
> acuan) — konsisten dengan semangat "best-effort" paragraf di bawah, hanya nama crate & versi
> preset yang berubah dari draf desain awal ini.

`rquest`/`wreq` (fork `reqwest` dengan dukungan TLS/JA3 impersonation) dipakai untuk HTTP client.
Acuan target Chrome 148 (User-Agent statis) tapi versi persis itu mungkin tidak tersedia
sebagai preset yang sudah dibundel — dipilih preset Chrome terbaru yang tersedia di versi yang
dipakai saat implementasi, DIDOKUMENTASIKAN di kode sebagai best-effort
(bukan jaminan match JA3/JA4 identik ke Chrome 148 asli), dengan catatan bahwa ini perlu
di-refresh berkala seiring cratenya update preset barunya. Header client-hints (`sec-ch-ua`,
`sec-ch-ua-platform`, dst.) tetap di-set manual mengikuti versi Chrome yang benar-benar
dipakai preset-nya (bukan hardcode 148 kalau presetnya beda), supaya UA header dan TLS
fingerprint konsisten satu sama lain.

## Struktur file

```
Backend/crates/spx-client/
  Cargo.toml            (rquest, aes-gcm, hkdf, sha2, argon2, secrecy, zeroize, rand, serde_json)
  src/
    lib.rs                    (re-export publik)
    booking.rs                 (SpxBooking struct penuh, normalize_booking, to_core_booking)
    accept.rs                   (AcceptReason enum, classify_accept_response + test)
    client.rs                    (SpxClient: http+cookies+headers, semua 10 endpoint)
    cookies.rs                    (SpxCookies struct, build_cookie_string, build_headers)
    crypto/
      mod.rs                       (re-export)
      secret.rs                     (SecretString wrapper alias/newtype convenience)
      envelope.rs                   (MasterKey, derive_subkey (HKDF), encrypt/decrypt (AES-GCM))
      password.rs                   (hash_password/verify_password, argon2id)
      session_token.rs              (generate_session_token, hash_session_token)
```

## Definition of Done — Fase 3

1. `spx-client::booking::normalize_booking` menghasilkan `SpxBooking` 29-field sesuai
   `spx.ts:116-195`, dan `to_core_booking` memetakan ke `core_domain::Booking` tanpa mengubah
   tipe Fase 1.
2. `classify_accept_response` lulus 8 kasus pesan retcode ASLI dari `spx-accept.test.ts`
   verbatim, plus kasus tambahan untuk urutan cek `agency_dup`-sebelum-`ok` (regression test
   untuk urutan yang salah bisa merusak deteksi agency_dup).
3. Envelope encryption: round-trip encrypt/decrypt lulus test, subkey purpose-scoped
   terverifikasi BEDA untuk `agency-credential` vs `waha-key` vs `quick-accept-hmac` walau
   dari master key yang sama (test langsung membandingkan byte subkey, bukan cuma "hasil
   encrypt beda").
4. **Ketiga gap keamanan tertutup, dibuktikan lewat test negatif**: (a) tidak ada
   `.pad`/`.slice` pada representasi key di mana pun kode kripto — key SELALU hasil HKDF
   32-byte penuh; (b) test yang membuktikan subkey AES != subkey HMAC secara konkret; (c)
   `site_settings` untuk WAHA key hanya berisi ciphertext+nonce, tidak pernah plaintext (test
   yang insert lalu assert kolom JSONB tidak mengandung substring key asli).
5. `cargo test -p spx-client` hijau, `cargo clippy -p spx-client -- -D warnings` bersih,
   `cargo deny check` bersih (rquest/aes-gcm/argon2/hkdf/secrecy/zeroize semua lisensi
   compatible).
6. `SecretString`/zeroize dipakai konsisten — `cargo tree` tidak menunjukkan jalur di mana
   password/master-key/session-token plaintext bisa lolos jadi `String` biasa yang gampang
   ke-log (code review point, bukan otomatis-terverifikasi lewat 1 command).
7. Password hashing pakai argon2id (bukan bcrypt) — test round-trip hash/verify plus test
   "hash yang sama dua kali menghasilkan output beda" (salt random per-hash, properti standar
   argon2id yang harus tetap ada).
8. Sesi: token plaintext 256-bit di-generate, HANYA hash SHA-256-nya yang pernah masuk DB
   (`portal_sessions.token_hash` dari Fase 2) — test yang insert session lalu assert
   `token_hash` di DB tidak sama dengan token asli dan tidak reversible (properti hash, bukan
   literally testable secara matematis, tapi minimal test "insert token asli sebagai token_hash
   akan gagal karena format/panjang beda" sebagai sanity check arah yang benar).
9. Keterbatasan fixture (tidak ada body SPX asli tersimpan di acuan) didokumentasikan eksplisit
   di kode/komentar test — bukan diam-diam diganti data sintetis tanpa catatan.

## Di luar cakupan (Fase 4+)

Dedup 3-layer (Redis Lua gate, in-proc claim) — itu `executor`, Fase 4. Poller state machine,
auto-login 3-tier menyeluruh (browser fallback via chromiumoxide) — `poller`, Fase 5 (Fase 3
hanya membangun HTTP client mentahnya, bukan orkestrasi login/polling). Rotasi master key
multi-versi — operasional Fase 8, hanya extension point (`key_version` kolom) yang disiapkan
sekarang.
