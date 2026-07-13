# Fase 4 — executor (3-layer dedup) design

Bagian dari [TOWER master spec](../../tower-master-spec.md). Sumber acuan fungsional:
`/tmp/spx-portal-ref` (clone lokal, sementara). File acuan persis yang dipakai untuk desain
ini (hasil riset mendalam):

- `apps/api/src/services/poller.ts` (2535 baris) — **tidak ada `executor.ts`/`dedup.ts`
  terpisah di acuan**; seluruh logic dedup+accept-dispatch hidup di sini, tercampur dengan
  poller. Fase 4 mengekstrak bagian accept-dispatch/dedup-nya jadi crate `executor` sendiri
  (sesuai `ARSITEKTUR` master spec yang MEMANG memisahkan `poller`/`executor` jadi 2 crate) —
  Fase 5 nanti bangun sisi poller-nya, memanggil `executor` sebagai library.
- `apps/api/src/services/spx.ts:450-467` (`fetchBookingAcceptor`, endpoint op-log —
  sudah di-port Fase 3 sebagai `SpxClient::fetch_bidding_log`).
- `apps/api/src/routes/bookings.ts:886-929` (manual accept route).

## Koreksi terhadap deskripsi master spec (temuan riset, bukan dugaan)

Master spec menyebut "3 layer" secara abstrak. Kode acuan sebenarnya:

1. **Bukan `DashMap`/`AtomicBool` CAS** — acuan pakai `Set<string>` JS biasa
   (`acceptingNow`, `acceptedIds`) yang aman hanya karena JS single-threaded. Port Rust-nya
   **wajib** struktur konkuren asli (lihat "Keputusan arsitektur" di bawah) — ini bukan
   penyimpangan dari acuan, tapi konsekuensi wajar port ke bahasa multi-thread.
2. **Bukan `EVALSHA`** — acuan pakai `redis.eval(fullScriptText, ...)` tiap panggilan (Lua
   script literal, bukan SHA yang di-cache). Master spec's sebutan "EVALSHA" tidak akurat
   untuk baris kode acuan. Port Rust ini **sengaja** memakai `SCRIPT LOAD` sekali +
   `EVALSHA` (dengan fallback `EVAL` kalau `NOSCRIPT`) — perbaikan performa yang align
   dengan Aturan Keras #1 (hot-path budget 1ms) dan justru memenuhi huruf literal master
   spec, walau acuan sendiri tidak melakukannya. Semantik identik, hanya transport call
   yang lebih murah.
3. **Kuota per-rule**, bukan quota global — `rule.conditions.maxAcceptCount`/`acceptedCount`
   (field yang sama seperti `core_domain::RuleConditions`/Fase 2's `accept_rules` kolom
   `max_accept_count`/`accepted_count`), dicek atomik di dalam Lua yang sama dengan claim.
4. **`withAccountLock` adalah in-proc promise-chain**, BUKAN distributed lock Redis
   (Redlock dsb). Port Rust: `tokio::sync::Mutex` per akun (async-aware), bukan lock Redis.
5. **`agency_dup` retry 0/500/1500ms — dikonfirmasi PERSIS benar** di kode acuan.
6. **"optimistik+unverified" di master spec sedikit berlebihan** — perilaku acuan nyata
   HANYA "jangan hitung sebagai kekalahan" (klasifikasi balik ke `ok`), TIDAK ada flag
   `unverified` eksplisit yang disimpan di mana pun. Desain ini mengikuti perilaku ASLI
   acuan (tidak ada flag unverified), bukan deskripsi master spec yang melebih-lebihkan.
7. **Fail-closed HANYA untuk jalur otomatis (poller).** Jalur manual accept (`beginManualAccept`)
   sengaja **fail-OPEN** kalau Redis error — rasionalnya: manual accept adalah aksi sadar
   manusia, dan gate poller yang fail-closed menjamin poller tidak akan bentrok berebut
   ticket yang sama secara bersamaan. Ini bukan inkonsistensi, tapi keputusan desain acuan
   yang disengaja — Fase 4 mem-port dua perilaku ini secara terpisah dan eksplisit, bukan
   menyamaratakan jadi satu.
8. **Manual accept BERBAGI Redis claim key** (`spx:claim:<acct>:<spxId>`) dengan jalur
   otomatis, tapi **TIDAK** menjalani cek kuota per-rule (manusia yang pilih tiketnya,
   kuota tidak relevan). Port Rust mempertahankan pembedaan ini persis.

## Tujuan

Bangun crate `executor` — dedup 3-layer + dispatch accept + verifikasi `agency_dup` —
sebagai library murni yang dipanggil Fase 5 (poller, orkestrasi siklus polling) dan Fase 6
(api-gateway, manual accept dari UI). Fase 4 sendiri TIDAK membangun poller/HTTP-route;
hanya fungsi-fungsi dedup/dispatch/verify yang keduanya akan panggil.

## Cakupan (in-scope)

- Layer 1 (in-proc, tercepat): claim in-flight + set accepted, per-akun, konkuren-aman.
- Layer 2 (Redis Lua, fail-closed untuk jalur otomatis): `ACCEPT_GATE_LUA` diporting persis
  (SET NX EX claim + cek kuota atomik + SADD inflight-set) via `SCRIPT LOAD`+`EVALSHA`.
- Layer 3 (durable, ZSET 7-hari): restore-sebelum-poll-pertama sebagai fungsi publik yang
  Fase 5 WAJIB `.await` sebelum menjadwalkan poll pertama (kontrak antar-fase, didokumentasikan
  eksplisit karena Fase 4 tidak memiliki "poll pertama" itu sendiri).
- `verify_agency_dup`: port persis retry 0/500/1500ms, heuristik pilih operator (email
  ber-`@`, tie-break `create_time` paling awal), banding lowercased/trimmed terhadap email
  akun sendiri (dari `SpxClient::fetch_profile`, Fase 3).
- Kuota re-read di dalam per-akun lock (`with_account_lock`) — port `applyRuleConsumption`'s
  race-fix persis: increment dari snapshot rule TERBARU (bukan snapshot lama pemanggil),
  persist, baru lepas slot in-flight kuota Redis.
- Fase 1 caveat (WAJIB, sudah dicatat di master spec): tambah
  `find_best_matching_rule_compiled(&[CompiledRule], &Booking, &MatchState) -> Option<usize>`
  ke `core_domain` — additive murni (tidak mengubah API/perilaku yang sudah ada), first-wins
  tie-break via loop manual (BUKAN `Iterator::max_by_key`, yang last-wins dan akan menyimpang
  dari acuan pada rule overlap ber-rank sama).
- Manual-accept variant (fail-open, berbagi claim key, skip cek kuota) — fungsi terpisah,
  dipanggil Fase 6 nanti, tapi dibangun sekarang karena berbagi state/keyspace dengan jalur
  otomatis (memisahkan ke fase lain berarti dua sumber kebenaran untuk keyspace yang sama).

## Di luar cakupan (Fase 5+)

Poller state machine penuh (single-flight per akun, notif watcher, fast-detect, hedged
fetch, anti-drift) — Fase 5. Auto-login 3-tier — Fase 5. API route untuk manual accept
(hanya fungsi executor-nya yang dibangun sekarang; endpoint HTTP-nya Fase 6). WA/push alert
delivery (`sendAgencyLossAlert`) — Fase 5's notifier; Fase 4 hanya expose sinyal
`AgencyDupOutcome::LostToAgency { rival_email }` yang notifier nanti konsumsi, tidak
mengirim WA sendiri (executor tidak boleh tahu cara kirim WA — pemisahan tanggung jawab).

## Keputusan arsitektur

### Layer 1: `DashMap`, bukan `Set` JS — CAS via `insert()`'s return value

`dashmap::DashSet<String>` (sharded, lock-per-shard) untuk `accepting_now` dan
`accepted_ids`, satu pasang PER AKUN (bukan satu DashSet global — mismatch skema kunci
antar-akun akan bikin akun A dan akun B saling collide di spxId yang sama, padahal SPX
mengizinkan spxId yang identik secara numerik muncul di akun berbeda — tidak umum tapi
tidak bisa diasumsikan mustahil). Struktur: `DashMap<AccountId, AccountDedupState>` dengan
`AccountDedupState { accepting_now: DashSet<String>, accepted_ids: DashSet<String> }`.

`DashSet::insert(key) -> bool` (true = baru ditambahkan, false = sudah ada) MEMBERIKAN
semantik atomic-check-and-set per-key secara langsung — ini port yang lebih kuat dari
acuan's `Set.has()`+`Set.add()` dua langkah (yang di JS aman cuma karena non-preemptive
single-thread; kalau di-port literal ke Rust dua langkah terpisah, ada race window di
antara `has()` dan `add()` kalau dipanggil dari >1 task Tokio bersamaan). `insert()`
tunggal menutup race ini by construction — bukan penyimpangan perilaku, koreksi bug laten
yang acuannya "kebetulan aman" karena bahasa sumbernya single-threaded.

Batas memori `accepted_ids` (acuan: 5000 entri, poller.ts:997-1000) — port sama: setelah
`insert` sukses, kalau `len() > 5000`, buang entri TERLAMA. `DashSet` tidak native
menyimpan urutan insersi — pakai `DashMap<String, Instant>` (bukan `DashSet<String>`
polos) untuk `accepted_ids` supaya bisa evict-oldest-first, trade-off kecil vs acuan's
`Set` (yang JS `Set` juga punya insertion-order iterasi native, jadi ini sebenarnya paritas,
bukan downgrade).

### Layer 2: Lua script — port verbatim, transport EVALSHA+SCRIPT LOAD

Script Lua-nya sendiri **diporting byte-for-byte** dari acuan (lihat riset di atas) — tidak
ada perubahan logic:

```lua
local ok = redis.call('SET', KEYS[1], '1', 'NX', 'EX', ARGV[4])
if not ok then return 0 end
local cap = tonumber(ARGV[2])
if cap > 0 then
  if redis.call('SISMEMBER', KEYS[2], ARGV[1]) == 0 and (tonumber(ARGV[3]) + redis.call('SCARD', KEYS[2])) >= cap then
    redis.call('DEL', KEYS[1])
    return -1
  end
  redis.call('SADD', KEYS[2], ARGV[1])
  redis.call('EXPIRE', KEYS[2], 600)
end
return 1
```

Key/arg shape identik: `KEYS[1]="spx:claim:<acct>:<spxId>"` (TTL 600s), `KEYS[2]="spx:inflight:<acct>:<ruleId|_norule>"`,
`ARGV = [spxId, cap, acceptedCount, "600"]`. Return `0`=sudah diklaim, `-1`=kuota penuh,
`1`=lanjut. Transport: `SCRIPT LOAD` sekali saat `ExecutorHandle` dibuat (simpan SHA1),
panggilan berikutnya `EVALSHA`; kalau Redis mengembalikan `NOSCRIPT` (mis. Redis di-restart
dan kehilangan cache script), fallback otomatis `SCRIPT LOAD` ulang + retry sekali. Ini
transparent bagi caller — tidak ada API tambahan, hanya optimisasi transport internal.

**Fail-closed**: SEMUA error dari panggilan Redis (koneksi gagal, timeout, NOSCRIPT setelah
retry gagal) di-map ke hasil "gate=0, jangan dispatch" — port persis `.catch(() => 0)`
acuan. Ini HANYA berlaku untuk `try_claim_auto` (jalur poller). `try_claim_manual` (lihat
di bawah) fail-OPEN, port persis `beginManualAccept`'s `catch` yang return `'ok'`.

### Layer 3: restore-before-first-poll — kontrak antar-fase eksplisit

`pub async fn restore_accepted_ids(redis: &RedisPool, account_id: &str, state: &AccountDedupState) -> Result<usize, ExecutorError>`
— trim ZSET ke jendela 7-hari (`ZREMRANGEBYSCORE 0 (now-7d)`), lalu `ZRANGE 0 -1` baca
semua member yang tersisa, masukkan ke `accepted_ids`. Return jumlah entri yang di-restore
(untuk observability/logging Fase 5).

**Kontrak yang WAJIB dihormati Fase 5**: fungsi ini harus di-`.await` SELESAI sebelum
menjadwalkan poll pertama akun tsb. Fase 4 sendiri tidak punya "loop poll" untuk
menegakkan ini secara paksa — didokumentasikan di doc-comment fungsi ini dengan huruf
kapital + dikutip alasan race acuan (CP-7, poller.ts:288-292) supaya Fase 5's implementer
tidak bisa melewatkannya tanpa sadar. Test Fase 4 hanya bisa membuktikan fungsi ini sendiri
benar (restore dari ZSET terisi menghasilkan `accepted_ids` yang benar); tidak bisa
membuktikan Fase 5 memanggilnya di urutan yang benar — itu jadi bagian Fase 5's DoD.

### Kuota per-akun: `with_account_lock` via `tokio::sync::Mutex`

`DashMap<AccountId, Arc<tokio::sync::Mutex<()>>>` — lookup-or-insert sekali per akun
(pakai `DashMap::entry().or_insert_with(...)`), lock `.await` sebelum baca-ulang+increment+
persist kuota. Ini FIFO per-akun (satu Tokio task nunggu giliran, bukan promise-chain JS,
tapi properti serialisasi-nya identik: tidak ada dua increment `acceptedCount` yang
overlap untuk akun yang sama). Urutan operasi di dalam lock (port `applyRuleConsumption`
persis): (1) re-read rule terbaru dari `store` (BUKAN snapshot pemanggil — inilah race
yang mau dicegah), (2) increment `accepted_count` pada salinan, (3) persist ke DB, (4)
BARU `SREM` slot in-flight Redis (`spx:inflight:<acct>:<ruleId>`, spxId) — urutan (3)
sebelum (4) memastikan count efektif tidak pernah "dip" (turun sesaat) yang bisa
membolehkan over-accept race lain.

### `verify_agency_dup` — port persis retry 0/500/1500ms

```rust
pub async fn verify_agency_dup(
    client: &SpxClient,
    cookies: &SpxCookies,
    self_email: &str,        // sudah lowercased/trimmed oleh caller (Layer di atas)
    booking_id: &str,
) -> AgencyDupOutcome
```

Loop `[0, 500, 1500]` ms (sleep SEBELUM percobaan ke-2/3, bukan sesudah — port persis).
Tiap percobaan panggil `SpxClient::fetch_bidding_log` (Fase 3, endpoint `log/list`),
filter `booking_operation_type == 4` (Accept), prioritaskan operator yang mengandung `@`,
di antara yang mengandung `@` pilih `create_time` PALING AWAL (bukan paling akhir — port
persis, karena op-log pertama yang tercatat adalah yang benar-benar menang race). Kalau
operator ber-`@` ditemukan: **berhenti retry**, bandingkan
`operator.to_lowercase().trim() == self_email` → `AgencyDupOutcome::Ours` (kita sendiri,
klasifikasi balik jadi `ok`) atau `AgencyDupOutcome::LostToAgency { rival_email }` (agency
lain menang, alert perlu dikirim Fase 5's notifier). Kalau 3 percobaan habis tanpa email
ber-`@`: `AgencyDupOutcome::Inconclusive` — **TIDAK ada flag "unverified" tersimpan di mana
pun**, caller (Fase 5) memperlakukan sama seperti `Ours` (klasifikasi balik `ok`) — port
persis perilaku acuan, BUKAN menambah state baru yang tidak ada di acuan.

### Manual-accept variant: fail-open, berbagi keyspace, skip kuota

```rust
pub async fn try_claim_manual(redis: &RedisPool, account_id: &str, spx_id: &str, dedup: &AccountDedupState) -> ManualClaimOutcome
```

Cek `dedup.accepted_ids`/`accepting_now` (Layer 1, sama seperti jalur otomatis) DAN
`ZSCORE` ZSET durable (Layer 3) — kalau salah satu positif, `ManualClaimOutcome::AlreadyAccepted`.
Kalau bersih, `SET spx:claim:<acct>:<spxId> 1 EX 600 NX` (KUNCI SAMA PERSIS dengan
`try_claim_auto`'s Layer 2 — inilah yang mencegah poller dan klik manual berebut ticket
yang sama). **TIDAK** memanggil `ACCEPT_GATE_LUA` (tidak ada cek kuota — manusia yang
pilih). Pada error Redis: **fail-OPEN**, return `ManualClaimOutcome::Ok` (port persis
`beginManualAccept`'s `catch(() => 'ok')`) — didokumentasikan tebal di doc-comment dengan
alasan acuan (aksi sadar manusia + poller's fail-closed gate menjamin tidak ada race
konkuren dari sisi poller).

### Fase 1 caveat: `find_best_matching_rule_compiled`

`Backend/crates/core-domain/src/matching.rs` mendapat SATU fungsi publik baru, murni
aditif (tidak mengubah fungsi/tipe yang sudah ada, tidak mengubah 127 test yang sudah
lulus):

```rust
pub fn find_best_matching_rule_compiled(
    rules: &[CompiledRule],
    booking: &Booking,
    state: &MatchState,
) -> Option<usize>
```

Beroperasi di atas `&[CompiledRule]` yang SUDAH di-compile (bukan `&[AcceptRule]` mentah
seperti `find_best_matching_rule` yang ada — itu tetap ada, tidak dihapus, dipakai di
tempat lain yang belum butuh hot-path). Tie-break WAJIB **first-wins** (loop manual
`for (i, r) in rules.iter().enumerate() { if r.matches(...) && (best.is_none() || r.rank > best_rank) { best = Some(i); best_rank = r.rank } }`
— strict `>`, BUKAN `>=`, supaya index PERTAMA yang capai rank tertinggi menang, bukan
yang terakhir) — `Iterator::max_by_key` TIDAK BOLEH dipakai di sini karena itu last-wins
pada tie, menyimpang dari acuan TS pada rule overlap ber-rank sama (persis peringatan
yang sudah dicatat di master spec dari review akhir Fase 1). Test WAJIB membuktikan
first-wins secara eksplisit: 2 `CompiledRule` dengan rank identik, assert index yang
dikembalikan adalah yang PERTAMA dalam slice, bukan yang terakhir.

`executor` crate memegang `Vec<CompiledRule>` sendiri per tenant (di-refresh saat rule
berubah — mekanisme refresh itu sendiri, cache-invalidation, TIDAK dibangun di Fase 4;
Fase 4 hanya menyediakan fungsi matching-nya dan menerima `&[CompiledRule]` dari caller —
siapa yang meng-compile-ulang saat rule di-update adalah keputusan Fase 6 (api-gateway,
tempat rule CRUD terjadi) atau Fase 5 (poller, kalau poll-time refresh dipilih) — di luar
scope Fase 4).

## Struktur file

```
Backend/crates/executor/
  Cargo.toml           (dashmap, redis (tokio-comp feature), tokio, thiserror,
                         core-domain, spx-client, store)
  src/
    lib.rs               (re-export publik)
    dedup.rs              (AccountDedupState, DashSet-based Layer 1, evict-oldest-5000)
    gate.rs                (ACCEPT_GATE_LUA verbatim, SCRIPT LOAD+EVALSHA+NOSCRIPT-fallback,
                             try_claim_auto (fail-closed), try_claim_manual (fail-open))
    restore.rs               (restore_accepted_ids — Layer 3, ZSET trim+read)
    account_lock.rs            (with_account_lock — tokio::sync::Mutex per akun)
    quota.rs                    (apply_rule_consumption — re-read-in-lock race fix)
    agency_dup.rs                (verify_agency_dup — retry 0/500/1500ms)

Backend/crates/core-domain/src/matching.rs   (+find_best_matching_rule_compiled, aditif)
```

## Definition of Done — Fase 4

1. Layer 1 (`DashSet`/`DashMap`-based `accepting_now`/`accepted_ids`) terbukti atomic:
   test yang menyerang `insert()` dari banyak task Tokio konkuren untuk spxId yang sama,
   assert TEPAT SATU yang berhasil claim (bukan race-prone `has()`+`add()` dua langkah).
2. `ACCEPT_GATE_LUA` diporting byte-for-byte identik dengan acuan (test yang membandingkan
   string literal-nya, bukan cuma perilakunya) — DAN perilaku 3 hasil (`0`/`-1`/`1`)
   terbukti benar lewat test terhadap Redis nyata (bukan mock) untuk: klaim baru (1),
   klaim duplikat (0), kuota penuh (-1).
3. Fail-closed (auto) vs fail-open (manual) keduanya terbukti lewat test yang mensimulasikan
   Redis unreachable (mis. koneksi ke port yang tidak listening) — auto HARUS menolak
   dispatch, manual HARUS tetap `Ok`.
4. Restore-before-first-poll: test yang isi ZSET manual, panggil `restore_accepted_ids`,
   assert `accepted_ids` berisi entri yang benar DAN entri di luar jendela 7-hari sudah
   ter-trim (tidak ikut ter-restore).
5. `verify_agency_dup`: test retry-timing (assert total durasi mendekati 0+500+1500=2000ms
   kalau semua percobaan gagal dapat email, bukan cuma assert hasil akhirnya), test
   early-stop (percobaan pertama sukses dapat email → tidak nunggu 500/1500ms lagi), test
   tie-break create_time paling awal.
6. `with_account_lock` + kuota re-read: test konkurensi yang menembak 2+ "accept" untuk
   rule yang sama secara bersamaan, assert `accepted_count` final benar (tidak lost-update)
   DAN tidak melebihi `max_accept_count`.
7. `find_best_matching_rule_compiled`: test first-wins eksplisit (2 rule rank sama, index
   pertama menang) + cross-check hasil identik dengan `find_best_matching_rule` yang sudah
   ada (non-hot-path) pada korpus booking/rule yang sama (membuktikan varian baru bukan
   fungsi yang berbeda perilakunya, cuma beda representasi input).
8. Manual accept terbukti berbagi Redis claim key dengan auto: test yang klaim via
   `try_claim_manual` untuk spxId X, lalu coba `try_claim_auto` untuk spxId X yang sama
   akun — HARUS gagal (key sudah diklaim), membuktikan keyspace benar-benar sama.
9. `cargo test -p executor` (+ `core-domain`'s tambahan test) hijau, `cargo clippy -p
   executor -- -D warnings` bersih, `cargo deny check` bersih, tidak ada dependency I/O
   yang tidak diharapkan selain `redis`/`store`/`spx-client`/`tokio` (tidak ada `reqwest`
   duplikat, tidak ada driver DB kedua).
