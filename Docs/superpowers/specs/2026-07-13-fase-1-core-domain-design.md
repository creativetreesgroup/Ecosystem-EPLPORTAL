# Fase 1 — core-domain: Rule Engine Port (design)

Bagian dari [TOWER master spec](../../tower-master-spec.md). Sumber acuan fungsional:
`/tmp/spx-portal-ref` (clone lokal, sementara, dari `creativetrees/Ecosystem-PortalSPX`
via `gh` CLI — lihat catatan di master spec). File acuan persis:

- `apps/api/src/services/matching.ts` (515 baris) + `apps/api/src/services/matching.test.ts` (549 baris)
- `apps/api/src/services/route.test.ts` (53 baris — subset fungsi parsing dari matching.ts)
- `apps/api/src/lib/coc.ts` (39 baris) + `apps/api/src/lib/coc.test.ts` (45 baris)
- `apps/api/src/services/spx.ts:7-37` (interface `SpxBooking` — hanya field yang benar-benar
  dipakai matcher yang di-port, lihat "Booking (subset)" di bawah)

## Tujuan

Port rule engine (money-critical: keputusan auto-accept) ke crate `core-domain` — **murni,
tanpa I/O** — dengan kesetaraan semantik line-for-line terhadap kode acuan, dan seluruh
~90 test acuan di-port jadi Rust test dan **wajib hijau semua**. Ini gate keras sebelum
Fase 2 boleh dimulai (skema DB), sesuai master spec.

## Cakupan (in-scope)

Semua fungsi publik `matching.ts` + `coc.ts`:

- `is_coc_name`, `is_coc`, `booking_type_of` (dari coc.ts)
- `norm_loc`, `loc_match` (whole-word location matching — anti `bali`⊄`Balikpapan`)
- `norm_vehicle`, `vehicle_match`, canonical vehicle label map
- `sanitize_accept_rules` (trim/canonicalize/cap-5-destinasi/conflict-resolve saat save)
- `matches_rule` (3 mode: booking_id, route, filter — termasuk shift/trip targeting,
  maxAcceptCount cap, guard rule-kosong-tidak-pernah-blanket-accept)
- `find_best_matching_rule` + rule ranking (mode dominan > priority > spesifisitas)
- `matched_booking_id_for` (HARUS pakai normalisasi identik dengan `matches_rule` — bug
  produksi historis yang dicegah test acuan)
- `dedupe_rules` (anti-duplikasi lane/booking-id, klaim ID prioritaskan rule enabled)
- `parse_route_stops`, `parse_route_detail_list` (parsing rute dari berbagai bentuk raw
  SPX — prioritas sumber: `route_stops` tersimpan > `route_detail_list` > `route_list` >
  `sgi_route_name` > `report_station_name` > origin/dest DC name)

## Di luar cakupan (Fase 3+)

Normalisasi booking SPX penuh (`normalizeBooking`), HTTP client, DB, Redis — semua itu
tinggal di `spx-client`/`store`/`executor` (Fase 2-4). `core-domain` Fase 1 hanya butuh
tipe `Booking` minimal yang dikonsumsi matcher (lihat di bawah), bukan `SpxBooking` penuh.

## Tipe kunci

### `Booking` (subset dari `SpxBooking`)

Field acuan (`spx.ts:7-37`) yang **benar-benar dibaca** oleh `matching.ts` (dikonfirmasi
dengan membaca setiap cabang `matches_rule`/`matched_booking_id_for`): `routeStops`,
`reportStation`, `spxTxId`, `bookingId`, `requestId`, `bookingType`, `vehicleType`,
`weight`, `codAmount`, `shiftType`, `tripType`. Field lain di `SpxBooking`
(`originRegion`/`originProvince`/`destinationRegion`/`destinationProvince`/`cod`/dst)
**sengaja tidak dibaca oleh logic matching** — test acuan
(`matching.test.ts:58-73`, "origin must NOT fall back to province/region labels")
justru membuktikan field itu diabaikan. Jadi `Booking` di Fase 1 hanya berisi field yang
dipakai; field SPX lain menyusul di Fase 3 saat `spx-client` membangun `SpxBooking` penuh
dan memetakannya ke `Booking` untuk dilempar ke matcher.

```rust
pub struct Booking {
    pub route_stops: Vec<String>,
    pub report_station: String,
    pub spx_tx_id: String,
    pub booking_id: String,
    pub request_id: String,
    pub booking_type: BookingType,   // enum Spxid | Reguler
    pub vehicle_type: String,
    pub weight: f64,
    pub cod_amount: f64,
    pub shift_type: i32,
    pub trip_type: i32,
}
```

### `AcceptRule` / `RuleMode` / `MatchMode` / `BookingTypeFilter`

Mirror `interface AcceptRule` TS persis (nama field snake_case). `mode` dan
`conditions.match_mode`/`conditions.booking_type` jadi enum Rust alih-alih string literal
union, supaya invalid state tidak representable — parity semantik tetap terjaga karena
`sanitize_accept_rules` di TS SUDAH memvalidasi/mem-fallback string-string itu ke nilai
yang sama dengan varian enum yang dipilih di sini.

### `MatchState`

```rust
pub struct MatchState {
    pub rule_accept_counts: HashMap<String, u32>,  // ganti Map<string, number> TS
}
```

### `CompiledRule` — pemenuhan requirement "precompute saat save"

Master spec: *"Compile rule ke bentuk ter-precompute (decision tree/bitset) saat save,
bukan evaluasi field-by-field per tiket."* Interpretasi yang diambil (YAGNI — belum ada
justifikasi volume data utk decision-tree/bitset penuh, itu bisa naik level nanti kalau
`find_best_matching_rule` linear-scan terbukti jadi bottleneck nyata di Fase 8 dengan data
produksi asli):

`CompiledRule::compile(&AcceptRule) -> CompiledRule` dipanggil SEKALI saat rule
disimpan/dimuat (bukan per tiket). Precompute: origin/destinations sudah di-`norm_loc`,
service types sudah di-canonical-label + `norm_vehicle`, rank-tuple sudah dihitung.
`matches(&self, booking: &Booking, state: &MatchState) -> bool` dan
`find_best_matching_rule(&[CompiledRule], ...)` beroperasi di atas bentuk precomputed ini
— tidak ada `to_lowercase()`/regex-replace berulang per tiket untuk field yang sama.

## Struktur file

```
Backend/crates/core-domain/
  Cargo.toml            (tambah dependency: tidak ada — regex-free, pure std)
  src/
    lib.rs               (re-export publik)
    coc.rs                (is_coc_name, is_coc, booking_type_of + test)
    booking.rs             (struct Booking, enum BookingType)
    rule.rs                 (AcceptRule, RuleMode, MatchMode, conditions, MatchState,
                             sanitize_accept_rules, dedupe_rules + test)
    location.rs              (norm_loc, loc_match + test)
    vehicle.rs                (norm_vehicle, vehicle_match, canonical label map + test)
    route_parse.rs              (parse_route_stops, parse_route_detail_list, RouteNode + test)
    matching.rs                  (CompiledRule::compile, matches, find_best_matching_rule,
                                  matched_booking_id_for + test — port matching.test.ts's
                                  matchesRule/findBestMatchingRule/matchedBookingIdFor groups)
```

File dipecah per tanggung jawab (mengikuti prinsip "satu file satu tujuan jelas") alih-alih
satu file 515-baris seperti acuan TS-nya — modul Rust lebih murah untuk dipecah karena
tidak ada biaya import bertingkat seperti file TS tunggal yang jadi kebiasaan JS.

## No I/O — satu dependency (serde_json), bukan nol

Normalisasi string (`norm_loc`, `norm_vehicle`, `norm_id`) tetap pure `std` — manual
char-iteration alih-alih `regex` crate (TS aslinya pakai regex tapi pattern-nya sederhana,
cukup dengan `char::is_ascii_alphanumeric()` manual), supaya bagian itu ringan dan cepat
compile. **Koreksi dari draf awal:** `parse_route_stops`/`parse_route_detail_list`
beroperasi di atas bentuk JSON longgar (raw SPX API shape) — menolak `serde_json` di sini
berarti hand-roll ulang sebuah JSON value enum yang toh akan dipakai lagi persis oleh
`spx-client` (Fase 3) dan `reactor-core` (sudah pakai `serde_json` sejak Fase 0). Jadi
`core-domain` punya **satu** dependency eksternal: `serde_json` (murni data-structure
library, bukan I/O — tidak melanggar "core-domain murni" dari master spec, yang melarang
`sqlx`/`reqwest`/`tokio`/`redis`, bukan semua crate eksternal apa pun). `sanitize_accept_rules`
tetap menerima tipe Rust longgar (`RawAcceptRule`/`RawRuleConditions` dengan field
`Option<T>`) alih-alih `serde_json::Value` mentah, karena bentuk `AcceptRule` sudah
terstruktur (nama field diketahui), tidak seperti payload SPX API yang benar-benar bebas
bentuk.

## Strategi test

Setiap `describe(...)` blok TS di-port jadi Rust `mod` berisi `#[test] fn` — nama test
diterjemahkan ke snake_case tapi tetap merujuk skenario yang sama (termasuk nomor
tiket/rule produksi asli yang disebut di komentar TS, supaya audit-trail ke insiden nyata
tidak hilang). Total ~90 assertion dari `matching.test.ts` + `route.test.ts` +
`coc.test.ts` harus punya padanan 1:1 — tidak ada test yang di-skip atau disederhanakan.

## Definition of Done — Fase 1

1. `cargo test -p core-domain` — semua ~90 test (port dari 3 file test acuan) hijau.
2. `cargo clippy -p core-domain -- -D warnings` bersih.
3. Tidak ada dependency I/O di `core-domain` (`cargo tree -p core-domain` hanya
   menunjukkan `serde_json` + transitive-nya sendiri — tidak ada `tokio`/`reqwest`/
   `sqlx`/`redis` di mana pun pada tree).
4. Setiap fungsi publik TS punya padanan Rust dengan nama snake_case yang jelas (lihat
   "Cakupan" di atas) — tidak ada fungsi yang "diringkas"/digabung tanpa alasan.
5. `CompiledRule` mendemonstrasikan precompute-at-save (bukan field-by-field re-parse per
   tiket) — dibuktikan lewat test yang memanggil `compile()` sekali lalu `matches()`
   berkali-kali dengan booking berbeda-beda.
