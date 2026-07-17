# Fase 7c (`/tickets` full management) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `/tickets` — a full booking management view (server-side filtered/paginated table of ALL bookings, live + historical, with a per-row detail drawer showing the accept-events audit trail and a manual-accept action) — reusing Fase 7b's WS store, nav shell, and design tokens.

**Architecture:** Two small, additive backend changes (a missing `route` field on `BookingDetail`, a new per-booking audit-trail endpoint) plus the FIRST dynamic-filter query pattern in the `store` crate (`sqlx::QueryBuilder`, parameterized throughout) added to the already-shipped `list_live`/`list_history`. On the frontend, a new `$lib/tickets.ts` (pure logic, its own `TicketRow`-equivalent type — deliberately NOT a reuse of `$lib/ticker.ts`'s simplified 3-status union, since `/tickets` needs the real `pending|accepted|failed` vocabulary plus failure sub-reasons) backs a `/tickets` page assembled from four new components: a reusable `Pagination`, a `TicketFilterBar`, a responsive `TicketsTable` (real `<table>` on desktop, stacked cards on narrow viewports — one component, not two), and a `TicketDetailDrawer`.

**Tech Stack:** Same as Fase 7a/7b (SvelteKit 2.69.2, Svelte 5 runes, Tailwind v4, `@lucide/svelte` for icons — no other new frontend dependencies). Backend: `sqlx` 0.9's `QueryBuilder` (already a workspace dependency, no new crate).

## Global Constraints

- Every color/font/radius in new `.svelte`/`.ts` files MUST come from the existing `--color-*`/`--font-*`/`--radius-*` tokens in `Frontend/src/app.css` — no raw hex. Status semantics already fixed project-wide: `--color-live` (teal) = accepted/healthy, `--color-accent` (amber) = pending/action, `--color-danger` (red) = failed.
- `sqlx::QueryBuilder` usage MUST parameterize every user-controlled value via `.push_bind(...)` — never string-interpolate a client-supplied value into the query text. This is the first dynamic-filter pattern in the `store` crate; get it right here since later sub-fases (Rules/Price/Activity) will likely copy it.
- The `status` filter query param is validated against the real, exhaustive vocabulary (`"pending" | "accepted" | "failed"`) server-side via `parse_status_filter` — reject anything else with `400 Bad Request`. Never trust a client string directly into a `WHERE status = ...` clause, validated or not — the validation function itself is what makes this safe, not the fact that it's bound.
- `$lib/tickets.ts`'s `TicketRow`-equivalent type is a NEW type, not imported from `$lib/ticker.ts` — the two views have genuinely different status vocabularies (`/command`'s ticker simplifies to `pending|accepted|taken_by_agency` for its live-only scope; `/tickets` needs the real `pending|accepted|failed` plus a `failureReason` sub-detail). Do not force a shared type across them.
- Route-text search (filtering by the SPX route-stop names) is explicitly OUT of scope for this plan — `route` has no DB index and no dynamic-filter precedent existed before this plan. Only `spx_id` (indexed), `status`, and `created_at` range are filterable. Do not add route search "while you're in there" — it's a deliberately deferred, disclosed gap (see the design doc), not an oversight to quietly fix.
- Pagination is server-side, numbered pages (`limit`/`offset`, reusing the existing convention) — no infinite scroll, no client-side virtualization library. Page size is capped at 200 (existing `clamp_limit`), which is small enough that virtualization adds no value here.
- `cargo fmt`/`cargo clippy --workspace --all-targets -- -D warnings`/`cargo test --workspace -- --test-threads=1` clean after every backend-touching task. `pnpm check` clean after every frontend-touching task.
- WCAG 2.2 AA: real `<table>`/`<th scope="col">` semantics on desktop (not a div-grid), every icon-only control gets `aria-label` or visually-hidden text, status badges are dot+text (never color alone), focus-visible rings and ≥44px tap targets on every interactive element, mobile card view carries the same information as its table row with visible field labels (not relying on column position).

---

## Task 1: Backend — `BookingDetail.route` field + per-booking audit-trail endpoint

**Files:**
- Modify: `Backend/crates/api-gateway/src/routes/bookings.rs`
- Modify: `Backend/crates/store/src/accept_events.rs`
- Modify: `Backend/crates/store/src/lib.rs`
- Modify: `Backend/crates/api-gateway/tests/bookings_routes.rs`

**Interfaces:**
- Produces (for Task 4): `BookingDetail.route: Vec<String>` (JSON key `route`, snake_case wire format — no `rename_all` anywhere in this crate, confirmed by grep) and `GET /bookings/{id}/audit-trail` → `Vec<AcceptEventItem>` (same shape `/bookings/spx-log` already returns).

- [ ] **Step 1: Add `route` to `BookingDetail`**

Read `Backend/crates/api-gateway/src/routes/bookings.rs` in full first (already summarized in this plan's research, but confirm nothing shifted). Then:

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — modify BookingDetail
#[derive(Debug, Serialize)]
pub struct BookingDetail {
    pub id: Uuid,
    pub account_id: String,
    pub spx_id: String,
    pub status: String,
    pub raw_data: Value,
    pub is_coc: bool,
    pub service_type: Option<String>,
    pub weight: f64,
    pub cod_amount: f64,
    pub auto_accepted: bool,
    pub accept_latency_ms: Option<i32>,
    pub rule_matched: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Same derivation as `BookingListItem.route` (Fase 7b Task 1) — computed from `raw_data`
    /// at read time, not a stored column. Added here because the detail drawer (Task 7) needs
    /// it and this field was missed when `BookingListItem` got it in 7b.
    pub route: Vec<String>,
}

impl From<store::models::Booking> for BookingDetail {
    fn from(b: store::models::Booking) -> Self {
        let route = spx_client::normalize_booking(&b.raw_data).route_stops;
        Self {
            id: b.id,
            account_id: b.account_id,
            spx_id: b.spx_id,
            status: b.status,
            raw_data: b.raw_data,
            is_coc: b.is_coc,
            service_type: b.service_type,
            weight: b.weight,
            cod_amount: b.cod_amount,
            auto_accepted: b.auto_accepted,
            accept_latency_ms: b.accept_latency_ms,
            rule_matched: b.rule_matched,
            created_at: b.created_at,
            updated_at: b.updated_at,
            route,
        }
    }
}
```

- [ ] **Step 2: Add `store::accept_events::list_for_booking`**

Read `Backend/crates/store/src/accept_events.rs` in full first. Then add, alongside the existing `list_for_tenant`:

```rust
// Backend/crates/store/src/accept_events.rs — add after list_for_tenant
/// Per-booking audit trail — `GET /bookings/:id/audit-trail` (Fase 7c). A single booking has
/// at most a handful of accept attempts (one per manual/auto try), so unlike `list_for_tenant`
/// this takes no `limit`/`offset` — there is no realistic case where pagination matters here.
pub async fn list_for_booking(
    pool: &PgPool,
    tenant_id: Uuid,
    booking_id: Uuid,
) -> Result<Vec<AcceptEvent>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows = sqlx::query_as::<_, AcceptEvent>(
        "SELECT id, tenant_id, booking_id, rule_id, outcome, local_dispatch_us, accept_e2e_ms, detail, created_at \
         FROM accept_events WHERE tenant_id = $1 AND booking_id = $2 ORDER BY created_at DESC",
    )
    .bind(tenant_id)
    .bind(booking_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}
```

Then, in `Backend/crates/store/src/lib.rs`, extend the existing re-export line:

```rust
// Backend/crates/store/src/lib.rs — modify this existing line
pub use accept_events::{
    insert as insert_accept_event, list_for_booking, list_for_tenant as list_accept_events,
    NewAcceptEvent,
};
```

- [ ] **Step 3: Add the `audit_trail` handler + route**

In `Backend/crates/api-gateway/src/routes/bookings.rs`, add a handler near `spx_log` (reusing the existing `AcceptEventItem`/`From<store::models::AcceptEvent>` already defined in this file — do not redefine it):

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — add after spx_log
/// `GET /bookings/:id/audit-trail` — the per-booking accept-attempt history (rule matched,
/// outcome, timing) for Task 7's detail drawer. `session_auth` only, same gate as every other
/// route in this router — this is per-booking data any logged-in tenant member should see,
/// matching `/bookings/spx-log`'s existing gate rather than `bot_log`'s stricter
/// `ManageBotSettings` permission (a different, coarser mechanism — see the design doc).
async fn audit_trail(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AcceptEventItem>>, ApiError> {
    let rows = store::accept_events::list_for_booking(&state.poller.pool, user.tenant_id, id).await?;
    Ok(Json(rows.into_iter().map(AcceptEventItem::from).collect()))
}
```

Register it in `bookings_router`:

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — modify bookings_router
pub fn bookings_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/live", get(live))
        .route("/history", get(history))
        .route("/{id}/detail", get(detail))
        .route("/{id}/audit-trail", get(audit_trail))
        .route("/spx-log", get(spx_log))
        .route("/{id}/accept", post(accept))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
```

- [ ] **Step 4: Write the failing tests**

Read `Backend/crates/api-gateway/tests/bookings_routes.rs`'s existing `detail_returns_full_raw_data_and_404s_for_unknown_id` and `spx_log_lists_accept_events_newest_first` tests in full for their exact seeding helpers (`insert_tenant`, `insert_portal_user`, `build_state`, `spawn_server`, `login_cookie`, `cleanup`, `store::upsert_booking`/`BookingUpsert`, `store::insert_accept_event`/`NewAcceptEvent`) — match their real signatures exactly, do not guess. Add two new tests:

```rust
// Backend/crates/api-gateway/tests/bookings_routes.rs — new tests
#[tokio::test]
async fn detail_includes_route_derived_from_raw_data() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "detail-route-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({
                "route_detail_list": [{
                    "node_info_list": [
                        {"name": "Cikarang DC", "address_info": {"l1": "Jabar", "l2": "Bekasi"}},
                    ]
                }]
            }),
        },
    )
    .await
    .expect("seed booking");

    let row = store::bookings::get_detail(&pool, tenant_id, /* fetch the id back */ {
        let live = store::bookings::list_live(&pool, tenant_id, 10, 0, &store::bookings::BookingFilter::default())
            .await
            .expect("list");
        live.iter().find(|b| b.spx_id == "detail-route-1").expect("seeded row").id
    })
    .await
    .expect("get_detail")
    .expect("row exists");
    assert_eq!(row.status, "pending"); // sanity check on the row itself, not the route below

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/{}/detail", row.id))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["route"], serde_json::json!(["Cikarang DC"]));

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn audit_trail_returns_only_this_bookings_events_tenant_scoped() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "audit-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking");
    let booking = {
        let live = store::bookings::list_live(&pool, tenant_id, 10, 0, &store::bookings::BookingFilter::default())
            .await
            .expect("list");
        live.into_iter().find(|b| b.spx_id == "audit-1").expect("seeded row")
    };

    store::insert_accept_event(
        &pool,
        tenant_id,
        &store::NewAcceptEvent {
            booking_id: Some(booking.id),
            rule_id: None,
            outcome: "accepted".to_string(),
            local_dispatch_us: Some(850),
            accept_e2e_ms: Some(312),
            detail: serde_json::json!({"manual": false}),
        },
    )
    .await
    .expect("seed accept_event for this booking");
    // A second, unrelated booking's event — must NOT show up in booking's own audit trail.
    store::insert_accept_event(
        &pool,
        tenant_id,
        &store::NewAcceptEvent {
            booking_id: None,
            rule_id: None,
            outcome: "failed".to_string(),
            local_dispatch_us: None,
            accept_e2e_ms: None,
            detail: serde_json::json!({}),
        },
    )
    .await
    .expect("seed unrelated accept_event");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/{}/audit-trail", booking.id))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1, "must only return this booking's own event, not the unrelated one");
    assert_eq!(body[0]["outcome"], "accepted");
    assert_eq!(body[0]["localDispatchUs"].as_i64().or(body[0]["local_dispatch_us"].as_i64()), Some(850));

    cleanup(&pool, tenant_id).await;
}
```

Note: the last assertion hedges on the exact JSON key casing (`localDispatchUs` vs `local_dispatch_us`) since `AcceptEventItem` has no `rename_all` — confirm the real casing when you run this (should be `local_dispatch_us`, matching the field name verbatim, consistent with `BookingListItem`'s snake_case) and simplify the assertion to the single correct key once confirmed; don't leave the `.or(...)` hedge in the final committed test.

- [ ] **Step 5: Run tests, then full workspace verification**

Run: `cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && unset DATABASE_URL && export REDIS_URL="redis://127.0.0.1:16379" && cargo test -p api-gateway --test bookings_routes -- --test-threads=1` — PASS (note: the first new test references `store::bookings::BookingFilter::default()`, which doesn't exist until Task 2 — if Task 1 is implemented before Task 2 lands, temporarily call `store::bookings::list_live(&pool, tenant_id, 10, 0)` with Task 1's pre-Task-2 signature instead; Task 2's implementer will need to update these two Task-1 tests' call sites when it changes `list_live`'s signature. Flag this cross-task dependency in your report.).

Run: `cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings` — clean.

- [ ] **Step 6: Commit**

```bash
git add Backend/crates/api-gateway/src/routes/bookings.rs Backend/crates/store/src/accept_events.rs \
        Backend/crates/store/src/lib.rs Backend/crates/api-gateway/tests/bookings_routes.rs
git commit -m "feat(api-gateway,store): BookingDetail.route field + per-booking audit-trail endpoint"
```

---

## Task 2: Backend — filter query params (status/spx_id/date-range) via `sqlx::QueryBuilder`

**Files:**
- Modify: `Backend/crates/store/src/bookings.rs`
- Modify: `Backend/crates/api-gateway/src/routes/bookings.rs`
- Modify: `Backend/crates/api-gateway/tests/bookings_routes.rs`

**Interfaces:**
- Produces (for Task 4): `GET /bookings/live` and `GET /bookings/history` accept new optional query params `status`, `spx_id`, `from`, `to`. Invalid `status` → `400 Bad Request`.
- Consumes: Task 1's `BookingFilter` type is introduced HERE (Task 1's tests reference `BookingFilter::default()` — see Task 1 Step 5's note; if Task 1 already landed with the old 4-arg `list_live`/`list_history` signature, this task updates those call sites too as part of its own diff).

- [ ] **Step 1: Add `BookingFilter` and rewrite `list_live`/`list_history` with `QueryBuilder`**

Read `Backend/crates/store/src/bookings.rs`'s current `list_live`/`list_history` in full first (already shown in this plan's research). Add near the top of the file:

```rust
// Backend/crates/store/src/bookings.rs — add near the top, after imports
use sqlx::QueryBuilder;

/// Optional filter conditions for `list_live`/`list_history` — the first dynamic-WHERE-clause
/// pattern in this crate. `status` is `&'static str` because callers must validate against the
/// real 3-value vocabulary BEFORE constructing this (see `api-gateway`'s `parse_status_filter`)
/// — this type intentionally cannot represent an invalid status, so validation can't be
/// forgotten at a call site.
#[derive(Debug, Default, Clone)]
pub struct BookingFilter {
    pub status: Option<&'static str>,
    pub spx_id: Option<String>,
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
}

/// Escapes `%`/`_`/`\` in a caller-supplied search term before it's embedded in a `LIKE`
/// pattern — without this, a user searching for a literal `%` or `_` in an spx_id would get
/// unintended wildcard matches. Not a SQL-injection concern (the value is still `push_bind`-ed,
/// never string-interpolated into the query) — this is purely about `LIKE` semantics.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}
```

Replace `list_live`:

```rust
// Backend/crates/store/src/bookings.rs — replace list_live
/// `/bookings/live`: pending bookings by default (or `filter.status` if set), newest first.
/// Uses the `idx_bookings_live_covering` index for the (typical) unfiltered/status-only case;
/// `spx_id`/date-range filters add extra predicates the planner evaluates after that index scan
/// (no additional index exists for those — acceptable at this table's expected volume, per the
/// design doc's "Parity dulu, optimasi kedua" scoping).
/// `limit`/`offset` are the caller's job to clamp to a sane range (the route layer does this).
pub async fn list_live(
    pool: &PgPool,
    tenant_id: Uuid,
    limit: i64,
    offset: i64,
    filter: &BookingFilter,
) -> Result<Vec<crate::models::Booking>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let mut qb = QueryBuilder::new(
        "SELECT id, tenant_id, account_id, spx_id, raw_data, status, is_coc, needs_enrichment, \
         service_type, weight, cod_amount, auto_accepted, accept_latency_ms, rule_matched, \
         created_at, updated_at FROM bookings WHERE tenant_id = ",
    );
    qb.push_bind(tenant_id);
    qb.push(" AND status = ");
    qb.push_bind(filter.status.unwrap_or("pending"));
    if let Some(spx_id) = &filter.spx_id {
        qb.push(" AND spx_id LIKE ");
        qb.push_bind(format!("{}%", escape_like(spx_id)));
        qb.push(" ESCAPE '\\'");
    }
    if let Some(from) = filter.from {
        qb.push(" AND created_at >= ");
        qb.push_bind(from);
    }
    if let Some(to) = filter.to {
        qb.push(" AND created_at <= ");
        qb.push_bind(to);
    }
    qb.push(" ORDER BY created_at DESC LIMIT ");
    qb.push_bind(limit);
    qb.push(" OFFSET ");
    qb.push_bind(offset);
    let rows = qb
        .build_query_as::<crate::models::Booking>()
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(rows)
}
```

Replace `list_history` the same way, with the default-status branch handling the `IN ('accepted', 'failed')` case:

```rust
// Backend/crates/store/src/bookings.rs — replace list_history
/// `/bookings/history`: terminal bookings (`accepted`/`failed` by default, or narrowed to just
/// `filter.status` if set), newest first. Uses the `idx_bookings_created_brin` BRIN index for
/// the time-ordered scan; same filter-cost caveat as `list_live` for `spx_id`/date-range.
pub async fn list_history(
    pool: &PgPool,
    tenant_id: Uuid,
    limit: i64,
    offset: i64,
    filter: &BookingFilter,
) -> Result<Vec<crate::models::Booking>, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let mut qb = QueryBuilder::new(
        "SELECT id, tenant_id, account_id, spx_id, raw_data, status, is_coc, needs_enrichment, \
         service_type, weight, cod_amount, auto_accepted, accept_latency_ms, rule_matched, \
         created_at, updated_at FROM bookings WHERE tenant_id = ",
    );
    qb.push_bind(tenant_id);
    match filter.status {
        Some(status) => {
            qb.push(" AND status = ");
            qb.push_bind(status);
        }
        None => {
            qb.push(" AND status IN ('accepted', 'failed')");
        }
    }
    if let Some(spx_id) = &filter.spx_id {
        qb.push(" AND spx_id LIKE ");
        qb.push_bind(format!("{}%", escape_like(spx_id)));
        qb.push(" ESCAPE '\\'");
    }
    if let Some(from) = filter.from {
        qb.push(" AND created_at >= ");
        qb.push_bind(from);
    }
    if let Some(to) = filter.to {
        qb.push(" AND created_at <= ");
        qb.push_bind(to);
    }
    qb.push(" ORDER BY created_at DESC LIMIT ");
    qb.push_bind(limit);
    qb.push(" OFFSET ");
    qb.push_bind(offset);
    let rows = qb
        .build_query_as::<crate::models::Booking>()
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(rows)
}
```

- [ ] **Step 2: Add `status`/`spx_id`/`from`/`to` to `ListParams` and validate `status`**

In `Backend/crates/api-gateway/src/routes/bookings.rs`:

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — modify ListParams
#[derive(Debug, Deserialize)]
pub struct ListParams {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// Optional status override — when present, replaces the endpoint's own default status
    /// set (`live` defaults to `pending`, `history` defaults to `accepted`+`failed`). Validated
    /// by `parse_status_filter` before use — never passed into SQL as a raw client string.
    pub status: Option<String>,
    /// Exact-or-prefix match on `spx_id` (uses the `(tenant_id, spx_id)` unique index).
    pub spx_id: Option<String>,
    /// Inclusive lower bound on `created_at`.
    pub from: Option<DateTime<Utc>>,
    /// Inclusive upper bound on `created_at`.
    pub to: Option<DateTime<Utc>>,
}

/// Validates a caller-supplied status filter against the real, exhaustive vocabulary
/// `bookings.status` ever takes (no DB CHECK constraint exists on this column — this
/// validation IS the enforcement; see `store::bookings`'s own writers for the 3 real values).
fn parse_status_filter(status: &str) -> Result<&'static str, ApiError> {
    match status {
        "pending" => Ok("pending"),
        "accepted" => Ok("accepted"),
        "failed" => Ok("failed"),
        other => Err(ApiError::BadRequest(format!("invalid status filter: {other}"))),
    }
}
```

- [ ] **Step 3: Wire the filter into `live`/`history` handlers**

```rust
// Backend/crates/api-gateway/src/routes/bookings.rs — modify live and history
async fn live(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<BookingListItem>>, ApiError> {
    let status = params.status.as_deref().map(parse_status_filter).transpose()?;
    let filter = store::bookings::BookingFilter {
        status,
        spx_id: params.spx_id.clone(),
        from: params.from,
        to: params.to,
    };
    let rows = store::bookings::list_live(
        &state.poller.pool,
        user.tenant_id,
        clamp_limit(params.limit),
        clamp_offset(params.offset),
        &filter,
    )
    .await?;
    Ok(Json(rows.into_iter().map(BookingListItem::from).collect()))
}

async fn history(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<BookingListItem>>, ApiError> {
    let status = params.status.as_deref().map(parse_status_filter).transpose()?;
    let filter = store::bookings::BookingFilter {
        status,
        spx_id: params.spx_id.clone(),
        from: params.from,
        to: params.to,
    };
    let rows = store::bookings::list_history(
        &state.poller.pool,
        user.tenant_id,
        clamp_limit(params.limit),
        clamp_offset(params.offset),
        &filter,
    )
    .await?;
    Ok(Json(rows.into_iter().map(BookingListItem::from).collect()))
}
```

If Task 1 landed first with its temporary 4-arg `list_live`/`list_history` calls in its own new tests (see Task 1 Step 5's note), update those two call sites to pass `&store::bookings::BookingFilter::default()` as part of this task's diff.

- [ ] **Step 4: Write the failing tests**

```rust
// Backend/crates/api-gateway/tests/bookings_routes.rs — new tests
#[tokio::test]
async fn history_status_filter_rejects_invalid_value_with_400() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/history?status=bogus"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn history_spx_id_filter_narrows_to_matching_prefix() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    for spx_id in ["filt-100", "filt-200", "other-300"] {
        store::upsert_booking(
            &pool,
            tenant_id,
            &store::BookingUpsert {
                account_id: "acct-1".to_string(),
                spx_id: spx_id.to_string(),
                status: "pending".to_string(),
                is_coc: false,
                raw_data: serde_json::json!({}),
            },
        )
        .await
        .expect("seed booking");
        store::update_booking_status(
            &pool,
            tenant_id,
            spx_id,
            store::BookingStatusUpdate {
                status: "accepted",
                latency_ms: Some(1),
                auto_accepted: true,
                rule_matched: None,
                accept_reason: None,
            },
        )
        .await
        .expect("mark accepted");
    }

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/history?spx_id=filt-"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    let spx_ids: Vec<&str> = body.iter().map(|b| b["spx_id"].as_str().unwrap()).collect();
    assert_eq!(spx_ids.len(), 2, "expected only the two filt-* rows, got {spx_ids:?}");
    assert!(spx_ids.contains(&"filt-100"));
    assert!(spx_ids.contains(&"filt-200"));
    assert!(!spx_ids.contains(&"other-300"));

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn history_status_filter_narrows_to_single_status() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner").await;

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "status-accepted-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking");
    store::update_booking_status(
        &pool,
        tenant_id,
        "status-accepted-1",
        store::BookingStatusUpdate {
            status: "accepted",
            latency_ms: Some(1),
            auto_accepted: true,
            rule_matched: None,
            accept_reason: None,
        },
    )
    .await
    .expect("mark accepted");

    store::upsert_booking(
        &pool,
        tenant_id,
        &store::BookingUpsert {
            account_id: "acct-1".to_string(),
            spx_id: "status-failed-1".to_string(),
            status: "pending".to_string(),
            is_coc: false,
            raw_data: serde_json::json!({}),
        },
    )
    .await
    .expect("seed booking");
    store::update_booking_status(
        &pool,
        tenant_id,
        "status-failed-1",
        store::BookingStatusUpdate {
            status: "failed",
            latency_ms: None,
            auto_accepted: false,
            rule_matched: None,
            accept_reason: Some("manual_accept_failed"),
        },
    )
    .await
    .expect("mark failed");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let resp = http
        .get(format!("{base}/bookings/history?status=failed"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["spx_id"], "status-failed-1");

    cleanup(&pool, tenant_id).await;
}
```

- [ ] **Step 5: Run it, then full workspace verification**

Run: `cargo test -p api-gateway --test bookings_routes -- --test-threads=1` — all PASS.
Run: `cargo test --workspace -- --test-threads=1 && cargo clippy --workspace --all-targets -- -D warnings` — clean. This task changes `list_live`/`list_history`'s public signature (new `filter` param) — grep the whole workspace for any OTHER caller of these two functions beyond `bookings.rs`'s `live`/`history` handlers and Task 1's two new tests before committing, to confirm nothing else breaks silently.

- [ ] **Step 6: Commit**

```bash
git add Backend/crates/store/src/bookings.rs Backend/crates/api-gateway/src/routes/bookings.rs \
        Backend/crates/api-gateway/tests/bookings_routes.rs
git commit -m "feat(store,api-gateway): status/spx_id/date-range filters on /bookings/live+history via QueryBuilder"
```

---

## Task 3: Frontend — `$lib/tickets.ts` pure logic (TDD)

**Files:**
- Create: `Frontend/src/lib/tickets.ts`
- Create: `Frontend/src/lib/tickets.test.ts`

**Interfaces:**
- Produces (for Tasks 4/6/7/8): `type TicketDetailRow`, `type TicketFilters`, `filtersToQueryString(filters, page, pageSize)`, `markRowAccepting(rows, id)`, `revertRowAccepting(rows, id)`, `applyRowAccepted(rows, id)`.

- [ ] **Step 1: Write the failing tests**

```typescript
// Frontend/src/lib/tickets.test.ts
import { describe, it, expect } from 'vitest';
import {
	filtersToQueryString,
	markRowAccepting,
	revertRowAccepting,
	applyRowAccepted,
	type TicketDetailRow
} from './tickets';

function row(overrides: Partial<TicketDetailRow> = {}): TicketDetailRow {
	return {
		id: 'row-uuid-1',
		spxId: 'SPX1',
		status: 'pending',
		failureReason: null,
		route: ['Jakarta', 'Bandung'],
		serviceType: 'Reguler',
		weight: 12.5,
		codAmount: 0,
		autoAccepted: false,
		createdAt: '2026-07-18T00:00:00Z',
		accepting: false,
		...overrides
	};
}

describe('filtersToQueryString', () => {
	it('omits empty/undefined filters entirely', () => {
		const qs = filtersToQueryString({ status: null, spxId: '', from: null, to: null }, 1, 50);
		expect(qs).toBe('limit=50&offset=0');
	});

	it('includes only the filters that are set, plus computed offset from page', () => {
		const qs = filtersToQueryString({ status: 'failed', spxId: 'SPX', from: null, to: null }, 3, 50);
		expect(qs).toContain('status=failed');
		expect(qs).toContain('spx_id=SPX');
		expect(qs).toContain('limit=50');
		expect(qs).toContain('offset=100');
	});

	it('includes from/to as ISO strings when set', () => {
		const qs = filtersToQueryString(
			{ status: null, spxId: '', from: '2026-07-01T00:00:00Z', to: '2026-07-18T00:00:00Z' },
			1,
			50
		);
		expect(qs).toContain('from=2026-07-01T00%3A00%3A00Z');
		expect(qs).toContain('to=2026-07-18T00%3A00%3A00Z');
	});
});

describe('markRowAccepting / revertRowAccepting / applyRowAccepted', () => {
	it('markRowAccepting sets accepting=true only on the matching row, returns a new array', () => {
		const rows = [row({ id: 'a' }), row({ id: 'b' })];
		const result = markRowAccepting(rows, 'a');
		expect(result).not.toBe(rows);
		expect(result.find((r) => r.id === 'a')?.accepting).toBe(true);
		expect(result.find((r) => r.id === 'b')?.accepting).toBe(false);
	});

	it('revertRowAccepting clears accepting on the matching row', () => {
		const rows = [row({ id: 'a', accepting: true })];
		const result = revertRowAccepting(rows, 'a');
		expect(result[0].accepting).toBe(false);
	});

	it('applyRowAccepted sets status=accepted and clears accepting on the matching row', () => {
		const rows = [row({ id: 'a', status: 'pending', accepting: true })];
		const result = applyRowAccepted(rows, 'a');
		expect(result[0].status).toBe('accepted');
		expect(result[0].accepting).toBe(false);
	});

	it('leaves non-matching rows byte-for-byte unchanged (same reference)', () => {
		const untouched = row({ id: 'b' });
		const rows = [row({ id: 'a' }), untouched];
		const result = markRowAccepting(rows, 'a');
		expect(result.find((r) => r.id === 'b')).toBe(untouched);
	});
});
```

- [ ] **Step 2: Run to verify failure**

```bash
cd Frontend && pnpm vitest run src/lib/tickets.test.ts
```
Expected: FAIL (`./tickets` module not found).

- [ ] **Step 3: Implement**

```typescript
// Frontend/src/lib/tickets.ts
// Pure logic for the /tickets full-management view — deliberately NOT a reuse of
// $lib/ticker.ts's TicketRow (that type's 3-value status union is correct for /command's
// live-only scope, wrong for this view's full pending|accepted|failed + sub-reason vocabulary).
// Every function returns a NEW array (never mutates), matching ticker.ts's own convention so
// Svelte 5's $state reassignment triggers reactivity correctly.

export type TicketStatus = 'pending' | 'accepted' | 'failed';
export type FailureReason = 'expired' | 'taken_by_other' | 'manual_accept_failed' | null;

export type TicketDetailRow = {
	id: string;
	spxId: string;
	status: TicketStatus;
	failureReason: FailureReason;
	route: string[];
	serviceType: string | null;
	weight: number;
	codAmount: number;
	autoAccepted: boolean;
	createdAt: string;
	/** True while an optimistic accept is in flight for this row. */
	accepting: boolean;
};

export type TicketFilters = {
	status: TicketStatus | null;
	spxId: string;
	from: string | null;
	to: string | null;
};

const PAGE_SIZE_DEFAULT = 50;

/** Maps 1-indexed `page` + `pageSize` to the backend's `limit`/`offset` convention, and only
 * includes filter params that are actually set — an omitted param means "no filter", not an
 * empty-string filter, matching the backend's `Option<T>` query-param semantics. */
export function filtersToQueryString(
	filters: Pick<TicketFilters, 'status' | 'spxId' | 'from' | 'to'>,
	page: number,
	pageSize: number = PAGE_SIZE_DEFAULT
): string {
	const params = new URLSearchParams();
	if (filters.status) params.set('status', filters.status);
	if (filters.spxId) params.set('spx_id', filters.spxId);
	if (filters.from) params.set('from', filters.from);
	if (filters.to) params.set('to', filters.to);
	params.set('limit', String(pageSize));
	params.set('offset', String((page - 1) * pageSize));
	return params.toString();
}

export function markRowAccepting(rows: TicketDetailRow[], id: string): TicketDetailRow[] {
	return rows.map((r) => (r.id === id ? { ...r, accepting: true } : r));
}

export function revertRowAccepting(rows: TicketDetailRow[], id: string): TicketDetailRow[] {
	return rows.map((r) => (r.id === id ? { ...r, accepting: false } : r));
}

export function applyRowAccepted(rows: TicketDetailRow[], id: string): TicketDetailRow[] {
	return rows.map((r) => (r.id === id ? { ...r, status: 'accepted' as const, accepting: false } : r));
}
```

- [ ] **Step 4: Run to verify all pass**

```bash
pnpm vitest run src/lib/tickets.test.ts
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
git add Frontend/src/lib/tickets.ts Frontend/src/lib/tickets.test.ts
git commit -m "feat(frontend): tickets.ts — pure logic for /tickets (filters, optimistic accept), unit-tested"
```

---

## Task 4: Frontend — `$lib/api-tickets.ts`

**Files:**
- Create: `Frontend/src/lib/api-tickets.ts`

**Interfaces:**
- Consumes: Task 3's `TicketDetailRow`/`TicketFilters`/`filtersToQueryString`; Fase 7b's `Frontend/src/lib/api-bookings.ts`'s `acceptBooking` (reused as-is — it's already a generic accept-by-id helper, not tied to `ticker.ts`); Fase 7a's `Frontend/src/lib/api.ts`'s `ApiError`.
- Produces (for Task 8): `fetchTickets(filters, page): Promise<{rows: TicketDetailRow[], hasMore: boolean}>`, `fetchBookingDetail(id): Promise<BookingDetail>`, `fetchAuditTrail(id): Promise<AuditEvent[]>`.

- [ ] **Step 1: Implement**

Read `Frontend/src/lib/api-bookings.ts` and `Frontend/src/lib/api.ts` in full first — match their exact wire-format conventions (snake_case REST JSON, confirmed by direct source reading in both files, not guessed).

```typescript
// Frontend/src/lib/api-tickets.ts
// Thin typed REST layer for /tickets — no UI logic here.
import { ApiError } from './api';
import { acceptBooking } from './api-bookings';
import { filtersToQueryString, type TicketDetailRow, type TicketFilters, type FailureReason } from './tickets';

export { acceptBooking };

// Wire shape of BookingListItem (snake_case — no rename_all anywhere in api-gateway, confirmed
// by reading Backend/crates/api-gateway/src/routes/bookings.rs directly). Only the fields this
// module reads are declared; extra JSON fields are ignored.
type BookingListItemWire = {
	id: string;
	spx_id: string;
	status: string;
	service_type: string | null;
	weight: number;
	cod_amount: number;
	auto_accepted: boolean;
	created_at: string;
	route: string[];
};

function failureReasonFromRaw(status: string, raw: Record<string, unknown> | undefined): FailureReason {
	if (status !== 'failed' || !raw) return null;
	const reason = raw['drift_reason'] ?? raw['accept_reason'];
	if (reason === 'expired' || reason === 'taken_by_other' || reason === 'manual_accept_failed') return reason;
	return null;
}

function toDetailRow(item: BookingListItemWire, failureReason: FailureReason = null): TicketDetailRow {
	return {
		id: item.id,
		spxId: item.spx_id,
		status: item.status as TicketDetailRow['status'],
		failureReason,
		route: item.route,
		serviceType: item.service_type,
		weight: item.weight,
		codAmount: item.cod_amount,
		autoAccepted: item.auto_accepted,
		createdAt: item.created_at,
		accepting: false
	};
}

const PAGE_SIZE = 50;

/** Routes to /live or /history (or both, merged) based on the status filter, per the design
 * doc's data-flow decision — /tickets stays a browse/search surface backed by the existing
 * two endpoints rather than a new merged one. Fetches one extra row beyond pageSize to compute
 * `hasMore` without a separate count query. */
export async function fetchTickets(
	filters: TicketFilters,
	page: number
): Promise<{ rows: TicketDetailRow[]; hasMore: boolean }> {
	const qs = filtersToQueryString(filters, page, PAGE_SIZE + 1);

	async function fetchOne(path: string): Promise<BookingListItemWire[]> {
		const res = await fetch(`${path}?${qs}`, { credentials: 'include' });
		if (!res.ok) throw new ApiError(res.status, `failed to fetch ${path}`);
		return res.json();
	}

	let items: BookingListItemWire[];
	if (filters.status === 'pending') {
		items = await fetchOne('/bookings/live');
	} else if (filters.status === 'accepted' || filters.status === 'failed') {
		items = await fetchOne('/bookings/history');
	} else {
		const [live, history] = await Promise.all([fetchOne('/bookings/live'), fetchOne('/bookings/history')]);
		items = [...live, ...history].sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
	}

	const hasMore = items.length > PAGE_SIZE;
	const pageItems = items.slice(0, PAGE_SIZE);
	return { rows: pageItems.map((item) => toDetailRow(item, null)), hasMore };
}

// Wire shape of BookingDetail (snake_case, includes raw_data for failureReason derivation).
type BookingDetailWire = {
	id: string;
	spx_id: string;
	status: string;
	raw_data: Record<string, unknown>;
	is_coc: boolean;
	service_type: string | null;
	weight: number;
	cod_amount: number;
	auto_accepted: boolean;
	accept_latency_ms: number | null;
	created_at: string;
	updated_at: string;
	route: string[];
};

export async function fetchBookingDetail(id: string): Promise<TicketDetailRow & { updatedAt: string; acceptLatencyMs: number | null; isCoc: boolean }> {
	const res = await fetch(`/bookings/${id}/detail`, { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch booking detail');
	const item: BookingDetailWire = await res.json();
	const failureReason = failureReasonFromRaw(item.status, item.raw_data);
	return {
		...toDetailRow(item, failureReason),
		updatedAt: item.updated_at,
		acceptLatencyMs: item.accept_latency_ms,
		isCoc: item.is_coc
	};
}

export type AuditEvent = {
	id: string;
	ruleId: string | null;
	outcome: string;
	localDispatchUs: number | null;
	acceptE2eMs: number | null;
	createdAt: string;
};

type AcceptEventItemWire = {
	id: string;
	rule_id: string | null;
	outcome: string;
	local_dispatch_us: number | null;
	accept_e2e_ms: number | null;
	created_at: string;
};

export async function fetchAuditTrail(id: string): Promise<AuditEvent[]> {
	const res = await fetch(`/bookings/${id}/audit-trail`, { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch audit trail');
	const items: AcceptEventItemWire[] = await res.json();
	return items.map((e) => ({
		id: e.id,
		ruleId: e.rule_id,
		outcome: e.outcome,
		localDispatchUs: e.local_dispatch_us,
		acceptE2eMs: e.accept_e2e_ms,
		createdAt: e.created_at
	}));
}
```

**Note on `failureReasonFromRaw` for `fetchTickets`:** `BookingListItem` (the `/live`/`/history` list wire shape) does NOT include `raw_data` — only `BookingDetail` does. This means the list view's rows cannot show a specific failure sub-reason badge from list data alone; `fetchTickets` passes `null` for every row's `failureReason` (see the call site above), and Task 6's table shows a generic "Gagal" (failed) label for `failed` rows in the LIST view, with the specific sub-reason (expired/taken-by-other/dispatch-failed) visible only after opening Task 7's detail drawer (which calls `fetchBookingDetail`, which DOES have `raw_data`). This is a real, disclosed scope simplification — flag it in your task report rather than silently working around it (e.g. do not add a new backend field to `BookingListItem` to fix this; that's out of this task's scope and not something the design doc asked for).

- [ ] **Step 2: Verify**

```bash
pnpm check
```
Expected: 0 errors.

- [ ] **Step 3: Commit**

```bash
git add Frontend/src/lib/api-tickets.ts
git commit -m "feat(frontend): api-tickets.ts — typed REST layer for /tickets"
```

---

## Task 5: Frontend — `Pagination.svelte` + `TicketFilterBar.svelte`

**Files:**
- Create: `Frontend/src/lib/components/Pagination.svelte`
- Create: `Frontend/src/lib/components/TicketFilterBar.svelte`

**Interfaces:**
- Produces (for Task 8): `<Pagination page={number} hasMore={boolean} onPageChange={(page: number) => void} />` — deliberately generic (no `/tickets`-specific naming), since Rules/Price/Activity will likely need the exact same control later (the one justified reusable-component call in this plan — the need is already visible across ≥2 upcoming surfaces).
- Produces: `<TicketFilterBar filters={TicketFilters} onFiltersChange={(filters: TicketFilters) => void} />`.

- [ ] **Step 1: `Pagination.svelte`**

```svelte
<!-- Frontend/src/lib/components/Pagination.svelte -->
<script lang="ts">
	import { ChevronLeft, ChevronRight } from '@lucide/svelte';

	let {
		page,
		hasMore,
		onPageChange
	}: { page: number; hasMore: boolean; onPageChange: (page: number) => void } = $props();
</script>

<nav class="flex items-center justify-between gap-3 py-2" aria-label="Navigasi halaman">
	<button
		type="button"
		disabled={page <= 1}
		onclick={() => onPageChange(page - 1)}
		aria-label="Halaman sebelumnya"
		class="min-h-[44px] min-w-[44px] flex items-center justify-center rounded-md border border-border text-text-muted hover:text-text-primary disabled:opacity-40 disabled:cursor-not-allowed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
	>
		<ChevronLeft size={18} aria-hidden="true" />
	</button>
	<span class="text-[12px] font-mono text-text-muted" aria-current="page">Halaman {page}</span>
	<button
		type="button"
		disabled={!hasMore}
		onclick={() => onPageChange(page + 1)}
		aria-label="Halaman berikutnya"
		class="min-h-[44px] min-w-[44px] flex items-center justify-center rounded-md border border-border text-text-muted hover:text-text-primary disabled:opacity-40 disabled:cursor-not-allowed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
	>
		<ChevronRight size={18} aria-hidden="true" />
	</button>
</nav>
```

- [ ] **Step 2: `TicketFilterBar.svelte`**

```svelte
<!-- Frontend/src/lib/components/TicketFilterBar.svelte -->
<script lang="ts">
	import { Search, X } from '@lucide/svelte';
	import type { TicketFilters, TicketStatus } from '$lib/tickets';

	let { filters, onFiltersChange }: { filters: TicketFilters; onFiltersChange: (f: TicketFilters) => void } =
		$props();

	const STATUS_OPTIONS: { value: TicketStatus | null; label: string }[] = [
		{ value: null, label: 'Semua status' },
		{ value: 'pending', label: 'Pending' },
		{ value: 'accepted', label: 'Diterima' },
		{ value: 'failed', label: 'Gagal' }
	];

	function updateStatus(e: Event) {
		const value = (e.target as HTMLSelectElement).value || null;
		onFiltersChange({ ...filters, status: value as TicketStatus | null });
	}

	function updateSpxId(e: Event) {
		onFiltersChange({ ...filters, spxId: (e.target as HTMLInputElement).value });
	}

	function updateFrom(e: Event) {
		const raw = (e.target as HTMLInputElement).value;
		onFiltersChange({ ...filters, from: raw ? new Date(raw).toISOString() : null });
	}

	function updateTo(e: Event) {
		const raw = (e.target as HTMLInputElement).value;
		onFiltersChange({ ...filters, to: raw ? new Date(raw).toISOString() : null });
	}

	function clearAll() {
		onFiltersChange({ status: null, spxId: '', from: null, to: null });
	}

	const hasActiveFilters = $derived(
		filters.status !== null || filters.spxId !== '' || filters.from !== null || filters.to !== null
	);
</script>

<div class="flex flex-wrap items-end gap-3 p-3 rounded-lg border border-border bg-bg-surface">
	<div class="flex flex-col gap-1">
		<label for="ticket-filter-status" class="text-[10px] font-body text-text-muted uppercase tracking-wide"
			>Status</label
		>
		<select
			id="ticket-filter-status"
			value={filters.status ?? ''}
			onchange={updateStatus}
			class="min-h-[44px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		>
			{#each STATUS_OPTIONS as opt (opt.value ?? 'all')}
				<option value={opt.value ?? ''}>{opt.label}</option>
			{/each}
		</select>
	</div>

	<div class="flex flex-col gap-1">
		<label for="ticket-filter-spxid" class="text-[10px] font-body text-text-muted uppercase tracking-wide"
			>SPX ID</label
		>
		<div class="relative">
			<Search size={14} aria-hidden="true" class="absolute left-2.5 top-1/2 -translate-y-1/2 text-text-muted" />
			<input
				id="ticket-filter-spxid"
				type="text"
				value={filters.spxId}
				oninput={updateSpxId}
				placeholder="Cari SPX ID"
				class="min-h-[44px] pl-8 pr-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary placeholder:text-text-muted focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			/>
		</div>
	</div>

	<div class="flex flex-col gap-1">
		<label for="ticket-filter-from" class="text-[10px] font-body text-text-muted uppercase tracking-wide">Dari</label>
		<input
			id="ticket-filter-from"
			type="date"
			value={filters.from ? filters.from.slice(0, 10) : ''}
			onchange={updateFrom}
			class="min-h-[44px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		/>
	</div>

	<div class="flex flex-col gap-1">
		<label for="ticket-filter-to" class="text-[10px] font-body text-text-muted uppercase tracking-wide">Sampai</label>
		<input
			id="ticket-filter-to"
			type="date"
			value={filters.to ? filters.to.slice(0, 10) : ''}
			onchange={updateTo}
			class="min-h-[44px] px-2.5 rounded-md text-[13px] font-body bg-bg-base border border-border text-text-primary focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		/>
	</div>

	{#if hasActiveFilters}
		<button
			type="button"
			onclick={clearAll}
			class="min-h-[44px] flex items-center gap-1.5 px-3 rounded-md text-[12px] font-body text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
		>
			<X size={14} aria-hidden="true" />
			Hapus filter
		</button>
	{/if}
</div>
```

- [ ] **Step 2: Run `pnpm check`, then commit**

```bash
cd Frontend && pnpm check
```
Expected: 0 errors.

```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
git add Frontend/src/lib/components/Pagination.svelte Frontend/src/lib/components/TicketFilterBar.svelte
git commit -m "feat(frontend): Pagination + TicketFilterBar components"
```

---

## Task 6: Frontend — `TicketsTable.svelte` (responsive table/card)

**Files:**
- Create: `Frontend/src/lib/components/TicketsTable.svelte`

**Interfaces:**
- Consumes: Task 3's `TicketDetailRow`.
- Produces (for Task 8): `<TicketsTable rows={TicketDetailRow[]} onRowClick={(row) => void} onAccept={(row) => void} />`.

- [ ] **Step 1: Implement**

```svelte
<!-- Frontend/src/lib/components/TicketsTable.svelte -->
<script lang="ts">
	// Real <table> on desktop (screen readers get real table navigation), stacked cards on
	// narrow viewports — ONE component, ONE source of row data, toggled via Tailwind's `md:`
	// breakpoint rather than two separate components that could drift out of sync.
	import type { TicketDetailRow } from '$lib/tickets';

	let {
		rows,
		onRowClick,
		onAccept
	}: {
		rows: TicketDetailRow[];
		onRowClick: (row: TicketDetailRow) => void;
		onAccept: (row: TicketDetailRow) => void;
	} = $props();

	function statusDotClass(status: TicketDetailRow['status']): string {
		if (status === 'accepted') return 'bg-live';
		if (status === 'failed') return 'bg-danger';
		return 'bg-accent';
	}

	function statusLabel(status: TicketDetailRow['status']): string {
		if (status === 'accepted') return 'Diterima';
		if (status === 'failed') return 'Gagal';
		return 'Pending';
	}

	function formatDate(iso: string): string {
		return new Date(iso).toLocaleString('id-ID', { dateStyle: 'medium', timeStyle: 'short' });
	}
</script>

{#if rows.length === 0}
	<div class="p-8 text-center text-[13px] font-body text-text-muted rounded-lg border border-border bg-bg-surface">
		Tidak ada tiket yang cocok dengan filter ini.
	</div>
{:else}
	<!-- Desktop: real table -->
	<table class="hidden md:table w-full text-[12px] font-body border-collapse">
		<caption class="sr-only">Daftar tiket booking</caption>
		<thead>
			<tr class="border-b border-border text-left text-[10px] uppercase tracking-wide text-text-muted">
				<th scope="col" class="py-2 pr-3">Status</th>
				<th scope="col" class="py-2 pr-3">SPX ID</th>
				<th scope="col" class="py-2 pr-3">Rute</th>
				<th scope="col" class="py-2 pr-3">Layanan</th>
				<th scope="col" class="py-2 pr-3 text-right">Berat</th>
				<th scope="col" class="py-2 pr-3 text-right">COD</th>
				<th scope="col" class="py-2 pr-3">Waktu</th>
				<th scope="col" class="py-2 pr-3"><span class="sr-only">Aksi</span></th>
			</tr>
		</thead>
		<tbody>
			{#each rows as row (row.id)}
				<tr class="border-b border-border hover:bg-bg-base cursor-pointer" onclick={() => onRowClick(row)}>
					<td class="py-2.5 pr-3">
						<span class="inline-flex items-center gap-1.5">
							<span aria-hidden="true" class="w-1.5 h-1.5 rounded-full shrink-0 {statusDotClass(row.status)}"></span>
							<span class="text-text-primary">{statusLabel(row.status)}</span>
						</span>
					</td>
					<td class="py-2.5 pr-3 font-mono text-text-muted">{row.spxId}</td>
					<td class="py-2.5 pr-3 text-text-primary truncate max-w-[220px]">{row.route.join(' → ') || '—'}</td>
					<td class="py-2.5 pr-3 text-text-muted">{row.serviceType ?? '—'}</td>
					<td class="py-2.5 pr-3 text-right font-mono text-text-muted">{row.weight.toFixed(1)} kg</td>
					<td class="py-2.5 pr-3 text-right font-mono text-text-muted">
						{row.codAmount > 0 ? row.codAmount.toLocaleString('id-ID') : '—'}
					</td>
					<td class="py-2.5 pr-3 font-mono text-text-muted whitespace-nowrap">{formatDate(row.createdAt)}</td>
					<td class="py-2.5 pr-3">
						{#if row.status === 'pending'}
							<button
								type="button"
								disabled={row.accepting}
								onclick={(e) => {
									e.stopPropagation();
									onAccept(row);
								}}
								class="min-h-[36px] px-2.5 rounded-md text-[11px] font-bold bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
							>
								{row.accepting ? 'Memproses…' : 'Terima'}
							</button>
						{/if}
					</td>
				</tr>
			{/each}
		</tbody>
	</table>

	<!-- Mobile: stacked cards, same information, visible field labels (column position is lost
	     once collapsed, so labels carry the meaning instead). -->
	<ul class="md:hidden flex flex-col gap-2">
		{#each rows as row (row.id)}
			<li>
				<button
					type="button"
					onclick={() => onRowClick(row)}
					class="w-full text-left p-3 rounded-lg border border-border bg-bg-surface flex flex-col gap-1.5 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
				>
					<div class="flex items-center justify-between">
						<span class="inline-flex items-center gap-1.5 text-[12px]">
							<span aria-hidden="true" class="w-1.5 h-1.5 rounded-full shrink-0 {statusDotClass(row.status)}"></span>
							<span class="text-text-primary">{statusLabel(row.status)}</span>
						</span>
						<span class="font-mono text-[11px] text-text-muted">{row.spxId}</span>
					</div>
					<div class="text-[12px] text-text-primary">{row.route.join(' → ') || '—'}</div>
					<div class="flex flex-wrap gap-x-3 gap-y-0.5 text-[11px] text-text-muted">
						<span>Layanan: {row.serviceType ?? '—'}</span>
						<span>Berat: {row.weight.toFixed(1)} kg</span>
						{#if row.codAmount > 0}<span>COD: {row.codAmount.toLocaleString('id-ID')}</span>{/if}
					</div>
					<div class="font-mono text-[10px] text-text-muted">{formatDate(row.createdAt)}</div>
					{#if row.status === 'pending'}
						<button
							type="button"
							disabled={row.accepting}
							onclick={(e) => {
								e.stopPropagation();
								onAccept(row);
							}}
							class="mt-1 min-h-[44px] w-full rounded-md text-[12px] font-bold bg-accent text-bg-base disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
						>
							{row.accepting ? 'Memproses…' : 'Terima'}
						</button>
					{/if}
				</button>
			</li>
		{/each}
	</ul>
{/if}
```

- [ ] **Step 2: Run `pnpm check`, then commit**

```bash
cd Frontend && pnpm check
```
Expected: 0 errors.

```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
git add Frontend/src/lib/components/TicketsTable.svelte
git commit -m "feat(frontend): TicketsTable — responsive table (desktop) / card list (mobile)"
```

---

## Task 7: Frontend — `TicketDetailDrawer.svelte`

**Files:**
- Create: `Frontend/src/lib/components/TicketDetailDrawer.svelte`

**Interfaces:**
- Consumes: Task 4's `fetchBookingDetail`/`fetchAuditTrail`.
- Produces (for Task 8): `<TicketDetailDrawer bookingId={string | null} onClose={() => void} />` — `bookingId === null` means closed (no drawer rendered); a real id triggers the fetch and slide-in panel.

- [ ] **Step 1: Implement**

```svelte
<!-- Frontend/src/lib/components/TicketDetailDrawer.svelte -->
<script lang="ts">
	import { X } from '@lucide/svelte';
	import { fetchBookingDetail, fetchAuditTrail, type AuditEvent } from '$lib/api-tickets';
	import type { TicketDetailRow } from '$lib/tickets';

	let { bookingId, onClose }: { bookingId: string | null; onClose: () => void } = $props();

	type DetailState = (TicketDetailRow & { updatedAt: string; acceptLatencyMs: number | null; isCoc: boolean }) | null;

	let detail = $state<DetailState>(null);
	let auditTrail = $state<AuditEvent[]>([]);
	let loading = $state(false);
	let errorMsg = $state('');

	$effect(() => {
		if (!bookingId) {
			detail = null;
			auditTrail = [];
			return;
		}
		loading = true;
		errorMsg = '';
		Promise.all([fetchBookingDetail(bookingId), fetchAuditTrail(bookingId)])
			.then(([d, events]) => {
				detail = d;
				auditTrail = events;
			})
			.catch(() => {
				errorMsg = 'Gagal memuat detail tiket.';
			})
			.finally(() => {
				loading = false;
			});
	});

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') onClose();
	}
</script>

<svelte:window onkeydown={bookingId ? handleKeydown : undefined} />

{#if bookingId}
	<div class="fixed inset-0 bg-black/40 z-40" onclick={onClose} aria-hidden="true"></div>
	<aside
		class="fixed right-0 top-0 bottom-0 w-full sm:w-[420px] bg-bg-surface border-l border-border z-50 overflow-y-auto p-4 flex flex-col gap-4"
		role="dialog"
		aria-label="Detail tiket"
		aria-modal="true"
	>
		<div class="flex items-center justify-between">
			<h2 class="font-heading font-bold text-text-primary text-sm">Detail Tiket</h2>
			<button
				type="button"
				onclick={onClose}
				aria-label="Tutup panel detail"
				class="min-w-[44px] min-h-[44px] flex items-center justify-center rounded-lg text-text-muted hover:text-text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent"
			>
				<X size={18} aria-hidden="true" />
			</button>
		</div>

		{#if loading}
			<p class="text-[12px] text-text-muted">Memuat…</p>
		{:else if errorMsg}
			<p role="alert" aria-live="polite" class="text-[12px] text-danger">{errorMsg}</p>
		{:else if detail}
			<dl class="grid grid-cols-2 gap-x-3 gap-y-2 text-[12px] font-body">
				<dt class="text-text-muted">SPX ID</dt>
				<dd class="font-mono text-text-primary">{detail.spxId}</dd>
				<dt class="text-text-muted">Status</dt>
				<dd class="text-text-primary">{detail.status}</dd>
				<dt class="text-text-muted">Rute</dt>
				<dd class="text-text-primary">{detail.route.join(' → ') || '—'}</dd>
				<dt class="text-text-muted">Layanan</dt>
				<dd class="text-text-primary">{detail.serviceType ?? '—'}</dd>
				<dt class="text-text-muted">Berat</dt>
				<dd class="font-mono text-text-primary">{detail.weight.toFixed(1)} kg</dd>
				<dt class="text-text-muted">COD</dt>
				<dd class="font-mono text-text-primary">{detail.codAmount > 0 ? detail.codAmount.toLocaleString('id-ID') : '—'}</dd>
				<dt class="text-text-muted">COC</dt>
				<dd class="text-text-primary">{detail.isCoc ? 'Ya' : 'Tidak'}</dd>
				<dt class="text-text-muted">Otomatis</dt>
				<dd class="text-text-primary">{detail.autoAccepted ? 'Ya' : 'Tidak'}</dd>
				{#if detail.acceptLatencyMs !== null}
					<dt class="text-text-muted">Latency</dt>
					<dd class="font-mono text-live">{detail.acceptLatencyMs}ms</dd>
				{/if}
			</dl>

			<div>
				<h3 class="font-heading font-bold text-text-primary text-[12px] mb-2">Riwayat Percobaan</h3>
				{#if auditTrail.length === 0}
					<p class="text-[11px] text-text-muted">Belum ada percobaan tercatat.</p>
				{:else}
					<ul class="flex flex-col gap-2">
						{#each auditTrail as event (event.id)}
							<li class="p-2.5 rounded-md border border-border text-[11px] font-body">
								<div class="flex justify-between">
									<span class="text-text-primary font-semibold">{event.outcome}</span>
									<span class="font-mono text-text-muted">{new Date(event.createdAt).toLocaleTimeString('id-ID')}</span>
								</div>
								{#if event.localDispatchUs !== null || event.acceptE2eMs !== null}
									<div class="font-mono text-text-muted mt-1">
										{#if event.localDispatchUs !== null}<span>decision: {(event.localDispatchUs / 1000).toFixed(2)}ms</span>{/if}
										{#if event.acceptE2eMs !== null}<span class="ml-2">e2e: {event.acceptE2eMs}ms</span>{/if}
									</div>
								{/if}
							</li>
						{/each}
					</ul>
				{/if}
			</div>
		{/if}
	</aside>
{/if}
```

- [ ] **Step 2: Run `pnpm check`, then commit**

```bash
cd Frontend && pnpm check
```
Expected: 0 errors.

```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
git add Frontend/src/lib/components/TicketDetailDrawer.svelte
git commit -m "feat(frontend): TicketDetailDrawer — booking detail + accept_events audit trail"
```

---

## Task 8: Frontend — `/tickets/+page.svelte` assembly

**Files:**
- Create: `Frontend/src/routes/(app)/tickets/+page.svelte`

**Interfaces:**
- Consumes: Tasks 3-7 (all of `tickets.ts`, `api-tickets.ts`, `Pagination`, `TicketFilterBar`, `TicketsTable`, `TicketDetailDrawer`); Fase 7b's WS store (`getContext<WsStore>('ws')`).

- [ ] **Step 1: Implement**

```svelte
<!-- Frontend/src/routes/(app)/tickets/+page.svelte -->
<script lang="ts">
	import { getContext, onMount } from 'svelte';
	import type { WsStore, TowerWsEvent } from '$lib/ws.svelte';
	import { fetchTickets, acceptBooking } from '$lib/api-tickets';
	import { markRowAccepting, revertRowAccepting, applyRowAccepted, type TicketDetailRow, type TicketFilters } from '$lib/tickets';
	import TicketFilterBar from '$lib/components/TicketFilterBar.svelte';
	import TicketsTable from '$lib/components/TicketsTable.svelte';
	import Pagination from '$lib/components/Pagination.svelte';
	import TicketDetailDrawer from '$lib/components/TicketDetailDrawer.svelte';
	import { ApiError } from '$lib/api';

	const ws = getContext<WsStore>('ws');

	let filters = $state<TicketFilters>({ status: null, spxId: '', from: null, to: null });
	let page = $state(1);
	let rows = $state<TicketDetailRow[]>([]);
	let hasMore = $state(false);
	let loading = $state(false);
	let errorMsg = $state('');
	let selectedBookingId = $state<string | null>(null);

	async function loadTickets() {
		loading = true;
		try {
			const result = await fetchTickets(filters, page);
			rows = result.rows;
			hasMore = result.hasMore;
			errorMsg = '';
		} catch {
			errorMsg = 'Gagal memuat daftar tiket. Coba lagi.';
		} finally {
			loading = false;
		}
	}

	function handleFiltersChange(next: TicketFilters) {
		filters = next;
		page = 1;
		loadTickets();
	}

	function handlePageChange(next: number) {
		page = next;
		loadTickets();
	}

	async function handleAccept(row: TicketDetailRow) {
		rows = markRowAccepting(rows, row.id);
		errorMsg = '';
		try {
			const result = await acceptBooking(row.id);
			if (!result.ok) {
				rows = revertRowAccepting(rows, row.id);
				errorMsg = result.message;
				return;
			}
			rows = applyRowAccepted(rows, row.id);
		} catch (e) {
			rows = revertRowAccepting(rows, row.id);
			if (e instanceof ApiError && e.status === 409) {
				errorMsg = 'Tiket ini sudah tidak tersedia — mungkin sudah diambil pihak lain.';
			} else if (e instanceof ApiError) {
				errorMsg = 'Server gagal memproses. Coba lagi.';
			} else {
				errorMsg = 'Tidak dapat menghubungi server. Coba lagi.';
			}
		}
	}

	function handleWsEvent(event: TowerWsEvent) {
		if (event.type === 'ticket_accepted') {
			rows = applyRowAccepted(rows, rows.find((r) => r.spxId === event.data.bookingId)?.id ?? '');
		}
	}

	onMount(() => {
		loadTickets();
		const unsubscribe = ws.onEvent(handleWsEvent);
		return () => unsubscribe();
	});
</script>

<svelte:head>
	<title>Tickets — TOWER</title>
</svelte:head>

<div class="p-4 flex flex-col gap-4 max-w-6xl mx-auto">
	<h1 class="font-heading font-bold text-text-primary text-lg">Tickets</h1>

	<TicketFilterBar {filters} onFiltersChange={handleFiltersChange} />

	{#if errorMsg}
		<div role="alert" aria-live="polite" class="px-3 py-2 rounded-md text-[12px] text-danger bg-danger/10 border border-border">
			{errorMsg}
		</div>
	{/if}

	{#if loading}
		<p class="text-[12px] text-text-muted">Memuat…</p>
	{:else}
		<TicketsTable {rows} onRowClick={(row) => (selectedBookingId = row.id)} onAccept={handleAccept} />
	{/if}

	<Pagination {page} {hasMore} onPageChange={handlePageChange} />
</div>

<TicketDetailDrawer bookingId={selectedBookingId} onClose={() => (selectedBookingId = null)} />
```

**Note on the WS reconciliation line:** `rows.find((r) => r.spxId === event.data.bookingId)?.id ?? ''` — if the WS event's booking isn't in the currently-loaded page (a real possibility, since `/tickets` is a filtered/paginated view, not a live feed), `applyRowAccepted` is called with an empty-string id that matches nothing, which is a safe no-op (confirmed by Task 3's `applyRowAccepted` implementation: `.map` over rows, no match = no change). This is intentional — `/tickets` does not attempt to keep off-page rows live-synced; the user re-filters/re-paginates to see current state, consistent with this being a browse/search surface rather than a live dashboard (that's `/command`'s job).

- [ ] **Step 2: Run `pnpm check` + unit tests, then commit**

```bash
cd Frontend && pnpm check && pnpm vitest run
```
Expected: 0 errors, all unit tests pass.

```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
git add "Frontend/src/routes/(app)/tickets/+page.svelte"
git commit -m "feat(frontend): /tickets page — filtered/paginated table + detail drawer + manual accept"
```

---

## Task 9: E2E test + final verification

**Files:**
- Create: `Frontend/tests/tickets.spec.ts`

**Interfaces:** none new — integration proof.

- [ ] **Step 1: Write the e2e test**

```typescript
// Frontend/tests/tickets.spec.ts
//
// Reuses the same real-stack setup as tests/login.spec.ts and tests/command.spec.ts — read
// those files' top comments for the full prerequisite list (reactor-core on :8081,
// TENANT_SLUG=tower-dev, seeded e2e-test-user). This file additionally needs at least one
// `accepted` and one `failed` booking seeded (beyond command.spec.ts's one `pending` row) so
// filtering by status has real data to distinguish — seed via the same direct-psql pattern,
// reading Backend/crates/store/migrations/ for the real schema before writing the seed command
// (do not guess column names).
import { test, expect } from '@playwright/test';

async function login(page: import('@playwright/test').Page) {
	await page.goto('/login');
	await page.getByLabel('Username').fill('e2e-test-user');
	await page.getByLabel('Password').fill('correct-horse-battery-staple');
	await page.getByRole('button', { name: 'Masuk ke Portal' }).click();
	await expect(page).toHaveURL(/\/command/);
}

test('unauthenticated visit to /tickets redirects to /login', async ({ page }) => {
	await page.goto('/tickets');
	await expect(page).toHaveURL(/\/login/);
});

test('after login, /tickets shows the seeded bookings in a table', async ({ page }) => {
	await login(page);
	await page.goto('/tickets');
	await expect(page.getByRole('table')).toBeVisible({ timeout: 10_000 });
});

test('filtering by status narrows the visible rows', async ({ page }) => {
	await login(page);
	await page.goto('/tickets');
	await expect(page.getByRole('table')).toBeVisible({ timeout: 10_000 });
	await page.getByLabel('Status').selectOption('pending');
	// After filtering to pending-only, no row should show a "Diterima" (accepted) status label.
	await expect(page.getByText('Diterima')).toHaveCount(0);
});

test('clicking a row opens the detail drawer with audit trail section', async ({ page }) => {
	await login(page);
	await page.goto('/tickets');
	await expect(page.getByRole('table')).toBeVisible({ timeout: 10_000 });
	await page.getByRole('row').nth(1).click();
	await expect(page.getByRole('dialog', { name: 'Detail tiket' })).toBeVisible();
	await expect(page.getByText('Riwayat Percobaan')).toBeVisible();
});

test('narrow viewport collapses the table into cards', async ({ page }) => {
	await page.setViewportSize({ width: 375, height: 800 });
	await login(page);
	await page.goto('/tickets');
	await expect(page.getByRole('table')).toBeHidden({ timeout: 10_000 });
});
```

- [ ] **Step 2: Seed the extra fixture data and run the e2e suite for real**

Prerequisites: `tower-postgres`/`tower-redis` up, `reactor-core` running locally, the pending booking from `command.spec.ts`'s seed still present, PLUS one `accepted` and one `failed` booking for THIS file's status-filter test — read `Backend/crates/store/migrations/0007_bookings.sql` for the real schema (generated columns, constraints) before writing the seed SQL, matching `command.spec.ts`'s own established discipline (do not guess).

```bash
cd Frontend && pnpm exec playwright test tests/tickets.spec.ts
```
Expected: all 5 tests pass. Investigate and root-cause-fix any failure — do not weaken assertions.

- [ ] **Step 3: Full backend + frontend verification**

```bash
cd Backend && export PATH="$HOME/.cargo/bin:$PATH" && unset DATABASE_URL && export REDIS_URL="redis://127.0.0.1:16379"
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
cd ../Frontend
pnpm check
pnpm vitest run
pnpm build
```
Expected: all genuinely green (the pre-existing `rate_limit.rs` real-wall-clock flake, if it recurs under machine load, is a known unrelated condition — see the Fase 7b progress ledger; re-run it in isolation to confirm before treating any failure as real).

- [ ] **Step 4: Commit**

```bash
cd /Users/halfirzzha/Documents/Server-Project/EPL-PROJECT
git add Frontend/tests/tickets.spec.ts
git commit -m "test(fase-7c): /tickets e2e (Playwright) — full workspace + frontend verification"
```

---

## Self-Review Notes (writing-plans skill, run by the plan author before handoff)

**Spec coverage:** every element of the approved design doc has a task — `BookingDetail.route` + audit-trail endpoint (Task 1), the QueryBuilder filter pattern (Task 2), pure logic + REST layer (Tasks 3-4), the reusable `Pagination` + `TicketFilterBar` (Task 5), the responsive `TicketsTable` (Task 6), `TicketDetailDrawer` with the audit trail (Task 7), page assembly (Task 8), e2e proof (Task 9). The explicitly-deferred route-text-search is NOT implemented anywhere in this plan — confirmed absent by design, matching the Global Constraints.

**Placeholder scan:** Task 1 Step 5 and Task 2 Step 3 both name a real, bounded cross-task dependency (Task 1's tests reference a type Task 2 introduces) with an explicit resolution path — not a vague TODO. Task 4's note on `failureReasonFromRaw` returning `null` for list-view rows is a disclosed, bounded scope simplification (BookingListItem lacks `raw_data`), not an oversight. Every other step has complete, runnable code.

**Type consistency:** `TicketDetailRow` (Task 3) is used identically by `api-tickets.ts` (Task 4), `TicketsTable.svelte` (Task 6), `TicketDetailDrawer.svelte` (Task 7 — as an extended type), and `/tickets/+page.svelte` (Task 8) — `id`/`spxId`/`status`/`failureReason`/`route`/`serviceType`/`weight`/`codAmount`/`autoAccepted`/`createdAt`/`accepting` field names never renamed between tasks. `BookingFilter` (Task 2, Rust) and `TicketFilters` (Task 3, TypeScript) are deliberately NOT the same shape — the Rust type is the validated, server-side representation; the TypeScript type is the raw UI-editable state that `filtersToQueryString` converts into request params, matching how `ListParams` (wire) and `BookingFilter` (validated) are already two different Rust types for the same reason.

**Cross-task dependency order:** Task 1 and Task 2 both touch `bookings.rs` and interact via the `BookingFilter`-vs-4-arg-signature seam flagged in both tasks' Steps — sequenced 1 then 2 with an explicit fallback note for the reverse order, matching Fase 7b's precedent for its Task 1/Task 2 pairing on the same file. Tasks 3-7 (frontend) can start once Task 4's wire-format assumptions are stable (verified against real, current backend source in this plan's own research, not guessed) — but per this project's subagent-driven-development convention, tasks still execute strictly serially regardless of this parallel-readiness, this note is for understanding only.
