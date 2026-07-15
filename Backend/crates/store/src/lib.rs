pub mod agency_credentials;
pub mod bookings;
pub mod models;
pub mod pool;
pub mod portal_sessions;
pub mod portal_users;
pub mod quota;
pub mod tenants;

pub use bookings::{
    expire_stale_bookings, resurrect_pending, update_booking_status, upsert_booking,
    BookingStatusUpdate, BookingUpsert, StaleOutcome,
};
pub use pool::{begin_tenant_tx, connect, run_migrations};
// `create`/`delete` are deliberately aliased rather than re-exported bare —
// `store::create`/`store::delete` would be an unhelpfully generic top-level
// name (and a future collision risk once other tenant-scoped modules land
// their own CRUD verbs); `find_valid_by_hash`/`touch_last_seen` are already
// unambiguous. Callers may also always reach these via the qualified
// `store::portal_sessions::...` path regardless (see the Fase 6a design
// doc's own middleware description, which uses that qualified form).
pub use portal_sessions::{
    create as create_portal_session, delete as delete_portal_session, find_valid_by_hash,
    touch_last_seen,
};
pub use portal_users::{find_by_id, find_by_username};
pub use quota::{consume_rule_quota, QuotaConsumeOutcome};
pub use tenants::find_by_slug;
// Re-export so downstream crates (e.g. executor) can name the pool type without
// a direct `sqlx` dependency.
pub use sqlx::PgPool;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_database_url() -> String {
        // 15432, not 5432 — see Docker/docker-compose.yml's tower-postgres port
        // comment: 5432 is often occupied by a pre-existing native Postgres on
        // the dev host, so the container's port is published on 15432 instead.
        std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
    }

    /// Inserts a throwaway tenant and returns its id. Callers are responsible
    /// for their own cleanup (existing tests `DELETE FROM tenants WHERE id =
    /// ...` at the end; `ON DELETE CASCADE` on tenant-scoped FKs means that
    /// also cleans up any dependent rows, e.g. bookings).
    async fn insert_test_tenant(pool: &sqlx::PgPool) -> uuid::Uuid {
        let tenant_id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
            .bind(tenant_id)
            .bind("Test Tenant")
            .bind(format!("test-{tenant_id}"))
            .execute(pool)
            .await
            .expect("insert tenant");
        tenant_id
    }

    /// `SET ROLE app_role` then begin a transaction with `app.tenant_id` set
    /// for its duration — the RLS-observing equivalent of `begin_tenant_tx`.
    ///
    /// This is NOT optional plumbing: `tower` (this crate's only configured
    /// Postgres login, `test_database_url()`'s default and this project's
    /// `Docker/docker-compose.yml` `POSTGRES_USER`) is a superuser with
    /// BYPASSRLS, and Postgres unconditionally exempts superusers from row
    /// security — `FORCE ROW LEVEL SECURITY` has zero effect on them. A test
    /// that ran tenant-scoped queries directly against `&pool`/`begin_tenant_tx`
    /// (as every other test in this module correctly does for non-RLS
    /// assertions) would therefore see ALL rows regardless of tenant,
    /// no matter how correct the RLS policy is — proving nothing. `app_role`
    /// (created NOLOGIN, no SUPERUSER/BYPASSRLS, in migration 0008; granted
    /// CRUD on the 12 non-append-only tenant tables in migration 0016) is
    /// genuinely subject to RLS, mirroring the discipline already
    /// established by `accept_events_is_append_only_for_app_role`.
    ///
    /// Caller must `RESET ROLE` on `conn` (then drop it) once done, so no
    /// role state bleeds onto whatever test next reuses this pooled
    /// connection — see call sites.
    async fn app_role_tenant_tx(
        conn: &mut sqlx::pool::PoolConnection<sqlx::Postgres>,
        tenant_id: uuid::Uuid,
    ) -> sqlx::Transaction<'_, sqlx::Postgres> {
        sqlx::query("SET ROLE app_role")
            .execute(&mut **conn)
            .await
            .expect("set role app_role");
        let mut tx = sqlx::Acquire::begin(conn)
            .await
            .expect("begin tx as app_role");
        sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
            .bind(tenant_id.to_string())
            .execute(&mut *tx)
            .await
            .expect("set tenant context");
        tx
    }

    #[tokio::test]
    async fn migrations_apply_and_tenant_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        let tenant_id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
            .bind(tenant_id)
            .bind("Test Tenant")
            .bind(format!("test-{tenant_id}"))
            .execute(&pool)
            .await
            .expect("insert tenant");

        let fetched: models::Tenant = sqlx::query_as("SELECT * FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .expect("fetch tenant");
        assert_eq!(fetched.name, "Test Tenant");

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Round-trips an `accept_rules` row and verifies two things about the
    /// `route_signature` generated column: (1) what Postgres actually
    /// computes for it, and (2) that the partial unique index
    /// `idx_accept_rules_route_dedup` fires on a second insert with the same
    /// tenant/mode/origin/destinations.
    ///
    /// NOTE on the expected signature: the generated-column expression
    /// mirrors `core_domain::dedupe_rules`'s 5-part signature exactly —
    /// `norm_loc(origin)|dests_sig|match_mode|booking_type|service_types_sig`
    /// — with BOTH `origin` and each `destinations` entry run through the
    /// same `norm_loc`-equivalent normalization (lowercase, collapse
    /// non-alphanumerics to a single space, trim). So for
    /// `origin = "Padang DC"`, `destinations = ["Cileungsi DC"]`, default
    /// `match_mode = 'strict'`, `booking_type = 'all'`, and an empty
    /// `service_types` (empty `service_types_sig`), the computed value is
    /// `"padang dc|cileungsi dc|strict|all|"` — note the trailing `|`
    /// separating `booking_type` from the (empty) `service_types_sig`.
    #[tokio::test]
    async fn accept_rule_route_signature_round_trips_and_dedup_index_fires() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        let tenant_id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
            .bind(tenant_id)
            .bind("Test Tenant")
            .bind(format!("test-{tenant_id}"))
            .execute(&pool)
            .await
            .expect("insert tenant");

        let first: models::AcceptRule = sqlx::query_as(
            "INSERT INTO accept_rules (tenant_id, name, mode, origin, destinations)
             VALUES ($1, $2, 'route', $3, $4)
             RETURNING *",
        )
        .bind(tenant_id)
        .bind("Padang -> Cileungsi")
        .bind("Padang DC")
        .bind(vec!["Cileungsi DC".to_string()])
        .fetch_one(&pool)
        .await
        .expect("insert first accept_rule");

        assert_eq!(
            first.route_signature.as_deref(),
            Some("padang dc|cileungsi dc|strict|all|")
        );

        let dup_result = sqlx::query(
            "INSERT INTO accept_rules (tenant_id, name, mode, origin, destinations)
             VALUES ($1, $2, 'route', $3, $4)",
        )
        .bind(tenant_id)
        .bind("Duplicate lane")
        .bind("Padang DC")
        .bind(vec!["Cileungsi DC".to_string()])
        .execute(&pool)
        .await;

        assert!(dup_result.is_err(), "second insert with same tenant/mode/origin/destinations should hit the dedup unique index");
        let err = dup_result.unwrap_err();
        let db_err = err.as_database_error().expect("expected a database error");
        assert_eq!(
            db_err.code().as_deref(),
            Some("23505"),
            "expected a unique_violation (23505), got: {db_err}"
        );

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Proves the `service_types_sig` fix: two `accept_rules` rows sharing
    /// the same origin/destinations/mode but with DIFFERENT `service_types`
    /// must both insert successfully — the route_signature's 5th segment
    /// (`service_types_sig`) makes them distinct lanes, so the partial
    /// unique index `idx_accept_rules_route_dedup` must NOT collide them.
    /// Before this fix (4-part signature, no `service_types` segment), this
    /// second insert would have wrongly failed with a unique violation.
    #[tokio::test]
    async fn accept_rule_route_dedup_distinguishes_by_service_types() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        let tenant_id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
            .bind(tenant_id)
            .bind("Test Tenant")
            .bind(format!("test-{tenant_id}"))
            .execute(&pool)
            .await
            .expect("insert tenant");

        let tronton: models::AcceptRule = sqlx::query_as(
            "INSERT INTO accept_rules (tenant_id, name, mode, origin, destinations, service_types)
             VALUES ($1, $2, 'route', $3, $4, $5)
             RETURNING *",
        )
        .bind(tenant_id)
        .bind("Padang -> Cileungsi (TRONTON)")
        .bind("Padang DC")
        .bind(vec!["Cileungsi DC".to_string()])
        .bind(vec!["TRONTON".to_string()])
        .fetch_one(&pool)
        .await
        .expect("insert TRONTON accept_rule should succeed");

        let fuso: models::AcceptRule = sqlx::query_as(
            "INSERT INTO accept_rules (tenant_id, name, mode, origin, destinations, service_types)
             VALUES ($1, $2, 'route', $3, $4, $5)
             RETURNING *",
        )
        .bind(tenant_id)
        .bind("Padang -> Cileungsi (FUSO)")
        .bind("Padang DC")
        .bind(vec!["Cileungsi DC".to_string()])
        .bind(vec!["FUSO".to_string()])
        .fetch_one(&pool)
        .await
        .expect("insert FUSO accept_rule should succeed — differing service_types must not collide with TRONTON's row");

        assert_ne!(
            tronton.route_signature, fuso.route_signature,
            "route_signature must differ when service_types differ"
        );
        assert_eq!(
            tronton.route_signature.as_deref(),
            Some("padang dc|cileungsi dc|strict|all|tronton")
        );
        assert_eq!(
            fuso.route_signature.as_deref(),
            Some("padang dc|cileungsi dc|strict|all|fuso")
        );

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Cross-checks the `bookings.is_coc` generated column against Fase 1's
    /// `core_domain::is_coc_name`/`is_coc` (`crates/core-domain/src/coc.rs`)
    /// on the exact same inputs its own test module covers, so the DB layer
    /// and the app layer can never silently diverge on this money-critical
    /// predicate:
    /// - `is_coc_name_spxid_prefix_rule`: SPXID-prefixed (upper/lower-case,
    ///   leading whitespace) -> true; non-SPXID (incl. SPXID mid-string, which
    ///   must NOT match since the predicate is anchored at the start) -> false.
    /// - `is_coc_from_either_identifier`: COC via booking_name even when
    ///   spx_id itself is a plain id -> true; neither identifier is SPXID
    ///   (including the empty-booking_name variant) -> false.
    #[tokio::test]
    async fn is_coc_generated_column_matches_core_domain_is_coc_name() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        // (spx_id, booking_name in raw_data, expected is_coc) — mirrors every
        // case in core-domain's coc.rs test module (spx_id gets a per-case
        // suffix below for uniqueness, which does not change whether it
        // starts with SPXID; booking_name is left byte-for-byte identical to
        // the Rust test's literals).
        let cases: &[(&str, &str, bool)] = &[
            ("SPXID12345", "", true),
            ("spxid-lower", "", true),
            ("  SPXID-leading-space", "", true),
            ("BK-778899", "", false),
            ("REGULER-1", "", false),
            ("MY-SPXID-suffix", "", false),
            ("884412771", "SPXID99887766", true), // COC via booking_name, not spx_id
            ("884412771", "BK-1", false),
            ("884412771", "", false), // neither identifier is SPXID (coc.rs: reg_when_neither_identifier_is_spxid)
        ];

        for (i, (spx_id, booking_name, expected)) in cases.iter().enumerate() {
            let unique_spx_id = format!("{spx_id}-case{i}");
            let raw_data = serde_json::json!({ "booking_name": booking_name });
            let row: (bool,) = sqlx::query_as(
                "INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, $2, $3) RETURNING is_coc",
            )
            .bind(tenant_id)
            .bind(&unique_spx_id)
            .bind(&raw_data)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|e| panic!("case {i} ({spx_id:?}, {booking_name:?}) insert failed: {e}"));
            assert_eq!(
                row.0, *expected,
                "case {i}: spx_id={spx_id:?} booking_name={booking_name:?}"
            );
        }

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Round-trips a `bookings` row through its typed `FromRow` struct
    /// (`models::Booking`). `bookings` already has substantial coverage via
    /// `is_coc_generated_column_matches_core_domain_is_coc_name` and the RLS
    /// tests, but none of those fetch a row back through `models::Booking`
    /// itself — they either check a single generated column via a raw tuple
    /// or only assert row counts. This is a small, separate test (rather
    /// than modifying the `is_coc` test, which has a distinct, focused
    /// purpose) that closes that gap, including the two Postgres-computed
    /// columns `is_coc`/`needs_enrichment`.
    #[tokio::test]
    async fn booking_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let raw_data = serde_json::json!({ "booking_name": "BK-ROUNDTRIP-1" });
        let inserted: models::Booking = sqlx::query_as(
            "INSERT INTO bookings (tenant_id, spx_id, raw_data, weight, cod_amount)
             VALUES ($1, $2, $3, $4, $5) RETURNING *",
        )
        .bind(tenant_id)
        .bind("BK-ROUNDTRIP-1")
        .bind(&raw_data)
        .bind(12.5_f64)
        .bind(50000.0_f64)
        .fetch_one(&pool)
        .await
        .expect("insert booking");

        let fetched: models::Booking = sqlx::query_as("SELECT * FROM bookings WHERE id = $1")
            .bind(inserted.id)
            .fetch_one(&pool)
            .await
            .expect("fetch booking");

        assert_eq!(fetched.tenant_id, tenant_id);
        assert_eq!(fetched.spx_id, "BK-ROUNDTRIP-1");
        assert_eq!(fetched.raw_data, raw_data);
        assert_eq!(fetched.status, "pending");
        assert!(
            !fetched.is_coc,
            "spx_id/booking_name here do not start with SPXID"
        );
        assert!(
            fetched.needs_enrichment,
            "no route_detail_list/route_stops supplied"
        );
        assert_eq!(fetched.weight, 12.5);
        assert_eq!(fetched.cod_amount, 50000.0);
        assert!(!fetched.auto_accepted);

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Proves `accept_events` is append-only at the DB permission level, not
    /// just by application convention. Table owners bypass GRANT/REVOKE
    /// entirely, so this test `SET ROLE app_role` before attempting the
    /// forbidden writes — otherwise it would pass for the wrong reason (as
    /// the table owner, not as the restricted role the app actually runs
    /// under once Task 7 wires up RLS + app_role for every tenant-scoped
    /// table).
    #[tokio::test]
    async fn accept_events_is_append_only_for_app_role() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let event_id: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO accept_events (tenant_id, outcome) VALUES ($1, 'accepted') RETURNING id",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("insert event");

        let mut conn = pool.acquire().await.expect("acquire");
        sqlx::query("SET ROLE app_role")
            .execute(&mut *conn)
            .await
            .expect("set role");

        let update_result =
            sqlx::query("UPDATE accept_events SET outcome = 'rejected' WHERE id = $1")
                .bind(event_id.0)
                .execute(&mut *conn)
                .await;
        assert!(
            update_result.is_err(),
            "app_role must not be able to UPDATE accept_events"
        );
        let update_err = update_result.unwrap_err();
        let update_db_err = update_err
            .as_database_error()
            .expect("expected a database error");
        assert_eq!(
            update_db_err.code().as_deref(),
            Some("42501"),
            "expected insufficient_privilege (42501) on UPDATE, got: {update_db_err}"
        );

        let delete_result = sqlx::query("DELETE FROM accept_events WHERE id = $1")
            .bind(event_id.0)
            .execute(&mut *conn)
            .await;
        assert!(
            delete_result.is_err(),
            "app_role must not be able to DELETE accept_events"
        );
        let delete_err = delete_result.unwrap_err();
        let delete_db_err = delete_err
            .as_database_error()
            .expect("expected a database error");
        assert_eq!(
            delete_db_err.code().as_deref(),
            Some("42501"),
            "expected insufficient_privilege (42501) on DELETE, got: {delete_db_err}"
        );

        sqlx::query("RESET ROLE").execute(&mut *conn).await.ok();
        drop(conn);

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Round-trips an `accept_events` row through its typed `FromRow` struct
    /// (`models::AcceptEvent`). `accept_events_is_append_only_for_app_role`
    /// already inserts a row but only ever fetches its `id` back via a raw
    /// tuple — its focus is proving UPDATE/DELETE are forbidden for
    /// `app_role`, not struct decoding, so this is a small separate test
    /// covering the typed-fetch path instead of modifying that one.
    #[tokio::test]
    async fn accept_event_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let detail = serde_json::json!({ "reason": "auto-accept rule matched" });
        let inserted: models::AcceptEvent = sqlx::query_as(
            "INSERT INTO accept_events (tenant_id, outcome, local_dispatch_us, accept_e2e_ms, detail)
             VALUES ($1, 'accepted', $2, $3, $4) RETURNING *",
        )
        .bind(tenant_id)
        .bind(1500_i64)
        .bind(42_i64)
        .bind(&detail)
        .fetch_one(&pool)
        .await
        .expect("insert accept_event");

        let fetched: models::AcceptEvent =
            sqlx::query_as("SELECT * FROM accept_events WHERE id = $1")
                .bind(inserted.id)
                .fetch_one(&pool)
                .await
                .expect("fetch accept_event");

        assert_eq!(fetched.tenant_id, tenant_id);
        assert_eq!(fetched.outcome, "accepted");
        assert_eq!(fetched.local_dispatch_us, Some(1500));
        assert_eq!(fetched.accept_e2e_ms, Some(42));
        assert_eq!(fetched.detail, detail);
        assert!(fetched.booking_id.is_none());
        assert!(fetched.rule_id.is_none());

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Aturan Keras #2 — `automation_settings.auto_accept_enabled` is a
    /// GLOBAL kill switch that must default to `false` at the schema level
    /// with zero application input. This inserts a row supplying ONLY
    /// `tenant_id` (every other column, including `auto_accept_enabled`, is
    /// left to its column default) and proves the fetched row can never come
    /// up `true`.
    #[tokio::test]
    async fn automation_settings_auto_accept_defaults_to_false() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        sqlx::query("INSERT INTO automation_settings (tenant_id) VALUES ($1)")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .expect("insert with only tenant_id, everything else default");

        let row: models::AutomationSettings =
            sqlx::query_as("SELECT * FROM automation_settings WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_one(&pool)
                .await
                .expect("fetch");
        assert!(
            !row.auto_accept_enabled,
            "kill switch must default to false with zero explicit input"
        );

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn notification_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let payload = serde_json::json!({ "title": "Booking accepted", "booking_id": "BK-1" });
        let inserted: models::Notification = sqlx::query_as(
            "INSERT INTO notifications (tenant_id, channel, payload) VALUES ($1, $2, $3) RETURNING *",
        )
        .bind(tenant_id)
        .bind("whatsapp")
        .bind(&payload)
        .fetch_one(&pool)
        .await
        .expect("insert notification");

        let fetched: models::Notification =
            sqlx::query_as("SELECT * FROM notifications WHERE id = $1")
                .bind(inserted.id)
                .fetch_one(&pool)
                .await
                .expect("fetch notification");

        assert_eq!(fetched.tenant_id, tenant_id);
        assert_eq!(fetched.channel, "whatsapp");
        assert_eq!(fetched.payload, payload);
        assert_eq!(fetched.status, "pending");
        assert_eq!(fetched.attempts, 0);
        assert!(fetched.sent_at.is_none());

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Round-trips a `portal_sessions` row through its typed `FromRow` struct
    /// — the "zero test coverage" gap flagged in the Fase 2 sign-off (Task
    /// 8): before this test, nothing ever fetched a `portal_sessions` row
    /// back through `models::PortalSession`, so a column-name/type mismatch
    /// between the migration and the struct (which `#[derive(FromRow)]`
    /// cannot catch at compile time) would have gone completely undetected.
    #[tokio::test]
    async fn portal_session_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let portal_user_id: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO portal_users (tenant_id, username, password_hash, display_name)
             VALUES ($1, 'agent-session', 'hash', 'Agent Session') RETURNING id",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("insert portal_user");

        let expires_at = chrono::Utc::now() + chrono::Duration::hours(2);
        sqlx::query(
            "INSERT INTO portal_sessions (tenant_id, portal_user_id, token_hash, ip, user_agent, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(tenant_id)
        .bind(portal_user_id.0)
        .bind(b"session-token-hash-bytes".to_vec())
        .bind("203.0.113.7")
        .bind("Mozilla/5.0 (test agent)")
        .bind(expires_at)
        .execute(&pool)
        .await
        .expect("insert portal_session");

        let fetched: models::PortalSession =
            sqlx::query_as("SELECT * FROM portal_sessions WHERE portal_user_id = $1")
                .bind(portal_user_id.0)
                .fetch_one(&pool)
                .await
                .expect("fetch portal_session");

        assert_eq!(fetched.tenant_id, tenant_id);
        assert_eq!(fetched.portal_user_id, portal_user_id.0);
        assert_eq!(fetched.token_hash, b"session-token-hash-bytes".to_vec());
        assert_eq!(fetched.ip.as_deref(), Some("203.0.113.7"));
        assert_eq!(
            fetched.user_agent.as_deref(),
            Some("Mozilla/5.0 (test agent)")
        );

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Round-trips an `agency_credentials` row through its typed `FromRow`
    /// struct — closes the "zero test coverage" gap from the Fase 2 sign-off.
    /// This table is what Fase 3 (spx-client + security kripto) builds
    /// encrypted-credential storage directly on top of, so an unverified
    /// struct-to-row mapping here (in particular the `ciphertext`/`nonce`
    /// `BYTEA` columns decoding cleanly into `Vec<u8>`) is a real risk to
    /// carry into the next phase.
    #[tokio::test]
    async fn agency_credential_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let ciphertext: Vec<u8> = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
        let nonce: Vec<u8> = vec![
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C,
        ];
        let inserted: models::AgencyCredential = sqlx::query_as(
            "INSERT INTO agency_credentials (tenant_id, label, username, ciphertext, nonce, key_version)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING *",
        )
        .bind(tenant_id)
        .bind("Main SPX Agency")
        .bind("agency-user-1")
        .bind(&ciphertext)
        .bind(&nonce)
        .bind(1_i32)
        .fetch_one(&pool)
        .await
        .expect("insert agency_credential");

        let fetched: models::AgencyCredential =
            sqlx::query_as("SELECT * FROM agency_credentials WHERE id = $1")
                .bind(inserted.id)
                .fetch_one(&pool)
                .await
                .expect("fetch agency_credential");

        assert_eq!(fetched.tenant_id, tenant_id);
        assert_eq!(fetched.label, "Main SPX Agency");
        assert_eq!(fetched.username, "agency-user-1");
        assert_eq!(fetched.ciphertext, ciphertext);
        assert_eq!(fetched.nonce, nonce);
        assert_eq!(fetched.key_version, 1);

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Round-trips a `rule_booking_targets` row through its typed `FromRow`
    /// struct — closes the "zero test coverage" gap from the Fase 2 sign-off.
    /// `rule_booking_targets.rule_id` FKs to `accept_rules(id)`, so this test
    /// first inserts a minimal parent `accept_rules` row (only its NOT NULL
    /// columns without defaults: `tenant_id`, `name`, `mode`) to satisfy that
    /// constraint.
    #[tokio::test]
    async fn rule_booking_target_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let rule_id: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO accept_rules (tenant_id, name, mode) VALUES ($1, $2, 'booking_id') RETURNING id",
        )
        .bind(tenant_id)
        .bind("Manual booking-id targets")
        .fetch_one(&pool)
        .await
        .expect("insert parent accept_rule");

        let inserted: models::RuleBookingTarget = sqlx::query_as(
            "INSERT INTO rule_booking_targets (tenant_id, rule_id, booking_id_raw, booking_id_norm)
             VALUES ($1, $2, $3, $4) RETURNING *",
        )
        .bind(tenant_id)
        .bind(rule_id.0)
        .bind("BK-778899")
        .bind("bk-778899")
        .fetch_one(&pool)
        .await
        .expect("insert rule_booking_target");

        let fetched: models::RuleBookingTarget =
            sqlx::query_as("SELECT * FROM rule_booking_targets WHERE id = $1")
                .bind(inserted.id)
                .fetch_one(&pool)
                .await
                .expect("fetch rule_booking_target");

        assert_eq!(fetched.tenant_id, tenant_id);
        assert_eq!(fetched.rule_id, rule_id.0);
        assert_eq!(fetched.booking_id_raw, "BK-778899");
        assert_eq!(fetched.booking_id_norm, "bk-778899");

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Round-trips a `portal_users` row through its typed `FromRow` struct
    /// (`models::PortalUser`). `portal_users` was previously only ever
    /// inserted as a throwaway FK parent for other tables (e.g.
    /// `push_subscription_round_trips`, fetched back as a raw `(Uuid,)`
    /// tuple), never fetched through its own typed struct.
    #[tokio::test]
    async fn portal_user_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let inserted: models::PortalUser = sqlx::query_as(
            "INSERT INTO portal_users (tenant_id, username, password_hash, display_name, is_main_account)
             VALUES ($1, $2, $3, $4, $5) RETURNING *",
        )
        .bind(tenant_id)
        .bind("main-agent")
        .bind("bcrypt-hash-value")
        .bind("Main Agent")
        .bind(true)
        .fetch_one(&pool)
        .await
        .expect("insert portal_user");

        let fetched: models::PortalUser =
            sqlx::query_as("SELECT * FROM portal_users WHERE id = $1")
                .bind(inserted.id)
                .fetch_one(&pool)
                .await
                .expect("fetch portal_user");

        assert_eq!(fetched.tenant_id, tenant_id);
        assert_eq!(fetched.username, "main-agent");
        assert_eq!(fetched.password_hash, "bcrypt-hash-value");
        assert_eq!(fetched.display_name, "Main Agent");
        assert!(fetched.is_main_account);
        assert!(fetched.enabled, "enabled must default to true");

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn push_subscription_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let portal_user_id: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO portal_users (tenant_id, username, password_hash, display_name)
             VALUES ($1, 'agent1', 'hash', 'Agent One') RETURNING id",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("insert portal_user");

        let expires_at = chrono::Utc::now() + chrono::Duration::days(30);
        let inserted: models::PushSubscription = sqlx::query_as(
            "INSERT INTO push_subscriptions (tenant_id, portal_user_id, endpoint, p256dh, auth, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING *",
        )
        .bind(tenant_id)
        .bind(portal_user_id.0)
        .bind("https://push.example.com/endpoint-1")
        .bind("p256dh-key")
        .bind("auth-secret")
        .bind(expires_at)
        .fetch_one(&pool)
        .await
        .expect("insert push_subscription");

        let fetched: models::PushSubscription =
            sqlx::query_as("SELECT * FROM push_subscriptions WHERE id = $1")
                .bind(inserted.id)
                .fetch_one(&pool)
                .await
                .expect("fetch push_subscription");

        assert_eq!(fetched.tenant_id, tenant_id);
        assert_eq!(fetched.portal_user_id, portal_user_id.0);
        assert_eq!(fetched.endpoint, "https://push.example.com/endpoint-1");
        assert_eq!(fetched.p256dh, "p256dh-key");
        assert_eq!(fetched.auth, "auth-secret");

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn site_setting_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let value = serde_json::json!({ "theme": "dark", "notify_sound": true });
        let inserted: models::SiteSetting = sqlx::query_as(
            "INSERT INTO site_settings (tenant_id, key, value) VALUES ($1, $2, $3) RETURNING *",
        )
        .bind(tenant_id)
        .bind("ui_preferences")
        .bind(&value)
        .fetch_one(&pool)
        .await
        .expect("insert site_setting");

        let fetched: models::SiteSetting =
            sqlx::query_as("SELECT * FROM site_settings WHERE tenant_id = $1 AND key = $2")
                .bind(tenant_id)
                .bind("ui_preferences")
                .fetch_one(&pool)
                .await
                .expect("fetch site_setting");

        assert_eq!(fetched.tenant_id, inserted.tenant_id);
        assert_eq!(fetched.key, inserted.key);
        assert_eq!(fetched.value, value);

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Proves the `route_prices_destinations_1to5` CHECK constraint: 0
    /// destinations must fail, 6 must fail, and anything in between (1-5)
    /// must succeed.
    #[tokio::test]
    async fn route_prices_destinations_check_enforces_1_to_5() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let insert = |destinations: serde_json::Value, code: String| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO route_prices (tenant_id, route_code, origin, destinations, price, vehicle_type) \
                     VALUES ($1, $2, 'Padang DC', $3, 100000, 'TRONTON')",
                )
                .bind(tenant_id)
                .bind(code)
                .bind(destinations)
                .execute(&pool)
                .await
            }
        };

        assert!(
            insert(serde_json::json!([]), "zero".into()).await.is_err(),
            "0 destinations must fail"
        );
        assert!(
            insert(
                serde_json::json!(["A", "B", "C", "D", "E", "F"]),
                "six".into()
            )
            .await
            .is_err(),
            "6 destinations must fail"
        );
        assert!(
            insert(serde_json::json!(["A", "B", "C"]), "three".into())
                .await
                .is_ok(),
            "3 destinations must succeed"
        );
        assert!(
            insert(serde_json::json!(["A"]), "one".into()).await.is_ok(),
            "1 destination must succeed"
        );
        assert!(
            insert(serde_json::json!(["A", "B", "C", "D", "E"]), "five".into())
                .await
                .is_ok(),
            "5 destinations must succeed"
        );

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Round-trips a `route_prices` row through its typed `FromRow` struct
    /// (`models::RoutePrice`). `route_prices_destinations_check_enforces_1_to_5`
    /// already inserts rows but only ever checks whether the INSERT
    /// succeeds/fails (raw `sqlx::query`, no struct decode) — its purpose is
    /// the CHECK constraint boundary, not struct decoding, so this is a
    /// small separate test rather than a modification of that one.
    #[tokio::test]
    async fn route_price_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let destinations = serde_json::json!(["Cileungsi DC", "Bekasi DC"]);
        let inserted: models::RoutePrice = sqlx::query_as(
            "INSERT INTO route_prices (tenant_id, route_code, region, origin, destinations, price, vehicle_type)
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
        )
        .bind(tenant_id)
        .bind("PDG-CLE-01")
        .bind("Sumatra")
        .bind("Padang DC")
        .bind(&destinations)
        .bind(150000_i64)
        .bind("TRONTON")
        .fetch_one(&pool)
        .await
        .expect("insert route_price");

        let fetched: models::RoutePrice =
            sqlx::query_as("SELECT * FROM route_prices WHERE id = $1")
                .bind(inserted.id)
                .fetch_one(&pool)
                .await
                .expect("fetch route_price");

        assert_eq!(fetched.tenant_id, tenant_id);
        assert_eq!(fetched.route_code, "PDG-CLE-01");
        assert_eq!(fetched.region, "Sumatra");
        assert_eq!(fetched.origin, "Padang DC");
        assert_eq!(fetched.destinations, destinations);
        assert_eq!(fetched.price, 150000);
        assert_eq!(fetched.vehicle_type, "TRONTON");

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn route_location_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let inserted: models::RouteLocation = sqlx::query_as(
            "INSERT INTO route_locations (tenant_id, name) VALUES ($1, $2) RETURNING *",
        )
        .bind(tenant_id)
        .bind("Padang DC")
        .fetch_one(&pool)
        .await
        .expect("insert route_location");

        let fetched: models::RouteLocation =
            sqlx::query_as("SELECT * FROM route_locations WHERE id = $1")
                .bind(inserted.id)
                .fetch_one(&pool)
                .await
                .expect("fetch route_location");

        assert_eq!(fetched.tenant_id, tenant_id);
        assert_eq!(fetched.name, "Padang DC");

        let dup_result =
            sqlx::query("INSERT INTO route_locations (tenant_id, name) VALUES ($1, $2)")
                .bind(tenant_id)
                .bind("Padang DC")
                .execute(&pool)
                .await;
        assert!(
            dup_result.is_err(),
            "duplicate (tenant_id, name) must violate the unique constraint"
        );

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn archive_run_round_trips() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        let inserted: models::ArchiveRun = sqlx::query_as(
            "INSERT INTO archive_runs (table_name, captured_count, archived_count, deleted_count) \
             VALUES ($1, $2, $3, $4) RETURNING *",
        )
        .bind("bookings")
        .bind(1000_i64)
        .bind(900_i64)
        .bind(900_i64)
        .fetch_one(&pool)
        .await
        .expect("insert archive_run");

        assert_eq!(
            inserted.status, "running",
            "status must default to 'running'"
        );
        assert!(!inserted.dry_run, "dry_run must default to false");
        assert!(inserted.archive_path.is_none());
        assert!(inserted.sha256.is_none());

        let fetched: models::ArchiveRun =
            sqlx::query_as("SELECT * FROM archive_runs WHERE id = $1")
                .bind(inserted.id)
                .fetch_one(&pool)
                .await
                .expect("fetch archive_run");

        assert_eq!(fetched.table_name, "bookings");
        assert_eq!(fetched.captured_count, 1000);
        assert_eq!(fetched.archived_count, 900);
        assert_eq!(fetched.deleted_count, 900);

        let bad_status = sqlx::query(
            "INSERT INTO archive_runs (table_name, captured_count, archived_count, deleted_count, status) \
             VALUES ($1, $2, $3, $4, 'bogus')",
        )
        .bind("bookings")
        .bind(0_i64)
        .bind(0_i64)
        .bind(0_i64)
        .execute(&pool)
        .await;
        assert!(
            bad_status.is_err(),
            "status must be constrained to running/completed/failed"
        );

        sqlx::query("DELETE FROM archive_runs WHERE id = $1")
            .bind(inserted.id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Proves RLS actually isolates tenants on `bookings`: tenant A can see
    /// its own row, tenant B (different `app.tenant_id`) sees nothing, and a
    /// query with NO tenant context set at all also sees nothing (not an
    /// error, not a leak) — matching `current_setting('app.tenant_id',
    /// true)`'s missing_ok semantics.
    ///
    /// Exercised via `app_role` (see `app_role_tenant_tx`), NOT via `&pool`/
    /// `begin_tenant_tx` directly — `tower` is a superuser and Postgres
    /// unconditionally exempts superusers from row security, so a version
    /// of this test that queried through the raw pool connection would
    /// observe tenant B (and the no-context case) seeing tenant A's row
    /// regardless of how correct the RLS policy is, proving nothing. (This
    /// is exactly what an earlier draft of this test did, and it correctly
    /// failed with `left: 1, right: 0` until switched to `app_role`.)
    #[tokio::test]
    async fn rls_blocks_cross_tenant_reads_on_bookings() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        let tenant_a = insert_test_tenant(&pool).await;
        let tenant_b = insert_test_tenant(&pool).await;

        // Insert a booking as tenant A.
        let mut conn_a = pool.acquire().await.expect("acquire a");
        {
            let mut tx_a = app_role_tenant_tx(&mut conn_a, tenant_a).await;
            sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, $2, '{}')")
                .bind(tenant_a)
                .bind("SPX-CROSS-TENANT-TEST")
                .execute(&mut *tx_a)
                .await
                .expect("insert as tenant a");
            tx_a.commit().await.expect("commit a");
        }
        sqlx::query("RESET ROLE").execute(&mut *conn_a).await.ok();
        drop(conn_a);

        // Tenant A can see its own row.
        let mut conn_a2 = pool.acquire().await.expect("acquire a2");
        {
            let mut tx_a2 = app_role_tenant_tx(&mut conn_a2, tenant_a).await;
            let seen_by_a: Vec<(uuid::Uuid,)> =
                sqlx::query_as("SELECT id FROM bookings WHERE spx_id = 'SPX-CROSS-TENANT-TEST'")
                    .fetch_all(&mut *tx_a2)
                    .await
                    .expect("select as tenant a");
            assert_eq!(seen_by_a.len(), 1, "tenant A must see its own booking");
            tx_a2.commit().await.ok();
        }
        sqlx::query("RESET ROLE").execute(&mut *conn_a2).await.ok();
        drop(conn_a2);

        // Tenant B must NOT see tenant A's row.
        let mut conn_b = pool.acquire().await.expect("acquire b");
        {
            let mut tx_b = app_role_tenant_tx(&mut conn_b, tenant_b).await;
            let seen_by_b: Vec<(uuid::Uuid,)> =
                sqlx::query_as("SELECT id FROM bookings WHERE spx_id = 'SPX-CROSS-TENANT-TEST'")
                    .fetch_all(&mut *tx_b)
                    .await
                    .expect("select as tenant b");
            assert_eq!(
                seen_by_b.len(),
                0,
                "tenant B must NOT see tenant A's booking — RLS leak"
            );
            tx_b.commit().await.ok();
        }
        sqlx::query("RESET ROLE").execute(&mut *conn_b).await.ok();
        drop(conn_b);

        // No tenant context at all, still as app_role (a role RLS actually
        // restricts): must also see nothing, not error.
        let mut conn_bare = pool.acquire().await.expect("acquire bare");
        sqlx::query("SET ROLE app_role")
            .execute(&mut *conn_bare)
            .await
            .expect("set role bare");
        let seen_bare: Vec<(uuid::Uuid,)> =
            sqlx::query_as("SELECT id FROM bookings WHERE spx_id = 'SPX-CROSS-TENANT-TEST'")
                .fetch_all(&mut *conn_bare)
                .await
                .expect("select with no tenant context");
        assert_eq!(
            seen_bare.len(),
            0,
            "queries with no tenant context set must see nothing, not error or leak"
        );
        sqlx::query("RESET ROLE")
            .execute(&mut *conn_bare)
            .await
            .ok();
        drop(conn_bare);

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_a)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_b)
            .execute(&pool)
            .await
            .ok();
    }

    /// Second cross-tenant probe on a distinct table shape: `site_settings`
    /// is keyed by (tenant_id, key) rather than a synthetic surrogate id, and
    /// its data (arbitrary JSONB config) is exactly the kind of thing that
    /// must never leak between tenants. This guards against a scenario where
    /// `bookings` alone happens to pass while the underlying RLS policy on a
    /// different table is missing or misconfigured. Same `app_role`
    /// discipline as `rls_blocks_cross_tenant_reads_on_bookings` and for the
    /// same reason: `tower` is a superuser and would bypass RLS entirely.
    #[tokio::test]
    async fn rls_blocks_cross_tenant_reads_on_site_settings() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        let tenant_a = insert_test_tenant(&pool).await;
        let tenant_b = insert_test_tenant(&pool).await;

        let mut conn_a = pool.acquire().await.expect("acquire a");
        {
            let mut tx_a = app_role_tenant_tx(&mut conn_a, tenant_a).await;
            sqlx::query(
                "INSERT INTO site_settings (tenant_id, key, value) VALUES ($1, 'secret_key', '{\"v\":1}')",
            )
            .bind(tenant_a)
            .execute(&mut *tx_a)
            .await
            .expect("insert as tenant a");
            tx_a.commit().await.expect("commit a");
        }
        sqlx::query("RESET ROLE").execute(&mut *conn_a).await.ok();
        drop(conn_a);

        let mut conn_b = pool.acquire().await.expect("acquire b");
        {
            let mut tx_b = app_role_tenant_tx(&mut conn_b, tenant_b).await;
            let seen_by_b: Vec<(uuid::Uuid,)> =
                sqlx::query_as("SELECT tenant_id FROM site_settings WHERE key = 'secret_key'")
                    .fetch_all(&mut *tx_b)
                    .await
                    .expect("select as tenant b");
            assert_eq!(
                seen_by_b.len(),
                0,
                "tenant B must NOT see tenant A's site_settings row — RLS leak"
            );
            tx_b.commit().await.ok();
        }
        sqlx::query("RESET ROLE").execute(&mut *conn_b).await.ok();
        drop(conn_b);

        let mut conn_bare = pool.acquire().await.expect("acquire bare");
        sqlx::query("SET ROLE app_role")
            .execute(&mut *conn_bare)
            .await
            .expect("set role bare");
        let seen_bare: Vec<(uuid::Uuid,)> =
            sqlx::query_as("SELECT tenant_id FROM site_settings WHERE key = 'secret_key'")
                .fetch_all(&mut *conn_bare)
                .await
                .expect("select with no tenant context");
        assert_eq!(
            seen_bare.len(),
            0,
            "queries with no tenant context set must see nothing, not error or leak"
        );
        sqlx::query("RESET ROLE")
            .execute(&mut *conn_bare)
            .await
            .ok();
        drop(conn_bare);

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_a)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_b)
            .execute(&pool)
            .await
            .ok();
    }

    /// Write-path counterpart to `rls_blocks_cross_tenant_reads_on_bookings`.
    ///
    /// `0016_rls_policies.sql`'s `tenant_isolation` policy has no explicit
    /// `FOR`/`WITH CHECK` clause. Per Postgres semantics, a policy with no
    /// `FOR` applies to ALL commands, and an omitted `WITH CHECK` on such a
    /// policy reuses the `USING` expression for the write-side check too —
    /// so `INSERT`/`UPDATE` are supposed to be just as constrained as
    /// `SELECT` is. Every other RLS test in this module only exercises the
    /// read path; without this test, a future refactor that silently added
    /// an explicit `WITH CHECK (true)` (or split the policy into a
    /// `FOR SELECT`-only one) would reopen cross-tenant write tagging with
    /// zero test failure, on the exact security centerpiece of Task 7.
    ///
    /// Exercised via `app_role` (see `app_role_tenant_tx`) for the same
    /// reason as every other RLS test here: `tower` is a superuser and
    /// Postgres unconditionally exempts superusers from row security, so a
    /// version of this test that inserted through the raw pool connection
    /// would succeed regardless of how correct the policy is, proving
    /// nothing.
    #[tokio::test]
    async fn rls_blocks_cross_tenant_writes_on_bookings() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        let tenant_a = insert_test_tenant(&pool).await;
        let tenant_b = insert_test_tenant(&pool).await;

        // Tenant A's session (app.tenant_id = A) attempts to INSERT a row
        // tagged tenant_id = B — a cross-tenant write-tagging attempt. The
        // policy's USING expression, reused for the INSERT's WITH CHECK,
        // must reject it.
        let mut conn_a = pool.acquire().await.expect("acquire a");
        {
            let mut tx_a = app_role_tenant_tx(&mut conn_a, tenant_a).await;
            let insert_result = sqlx::query(
                "INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, $2, '{}')",
            )
            .bind(tenant_b)
            .bind("SPX-CROSS-TENANT-WRITE-TEST")
            .execute(&mut *tx_a)
            .await;
            assert!(
                insert_result.is_err(),
                "app_role in tenant A's context must not be able to INSERT a row tagged tenant_id = B"
            );
            let insert_err = insert_result.unwrap_err();
            let insert_db_err = insert_err
                .as_database_error()
                .expect("expected a database error");
            assert_eq!(
                insert_db_err.code().as_deref(),
                Some("42501"),
                "expected insufficient_privilege (42501) row-security-policy violation, got: {insert_db_err}"
            );
            assert!(
                insert_db_err
                    .message()
                    .contains("row-level security policy"),
                "expected a row-level security policy violation message, got: {}",
                insert_db_err.message()
            );
            tx_a.rollback().await.ok();
        }
        sqlx::query("RESET ROLE").execute(&mut *conn_a).await.ok();
        drop(conn_a);

        // Control: the same tenant A context legitimately inserting a row
        // tagged tenant_id = A must still succeed — proving the rejection
        // above is specifically about the tenant mismatch, not some other
        // failure (bad connection, missing grant, etc.).
        let mut conn_a2 = pool.acquire().await.expect("acquire a2");
        {
            let mut tx_a2 = app_role_tenant_tx(&mut conn_a2, tenant_a).await;
            sqlx::query("INSERT INTO bookings (tenant_id, spx_id, raw_data) VALUES ($1, $2, '{}')")
                .bind(tenant_a)
                .bind("SPX-CROSS-TENANT-WRITE-TEST")
                .execute(&mut *tx_a2)
                .await
                .expect("legitimate same-tenant insert must succeed");
            tx_a2.commit().await.expect("commit a2");
        }
        sqlx::query("RESET ROLE").execute(&mut *conn_a2).await.ok();
        drop(conn_a2);

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_a)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_b)
            .execute(&pool)
            .await
            .ok();
    }

    /// Regression guard for the `FORCE ROW LEVEL SECURITY` requirement
    /// itself: queries `pg_class.relforcerowsecurity` for a sample of the 13
    /// tables so a future migration edit that drops `FORCE` (leaving only
    /// `ENABLE`) fails this test immediately instead of silently
    /// reintroducing an owner-bypass hole. `ENABLE` alone does not restrict
    /// the table owner, and the test's own connection IS the owner (it ran
    /// the migrations), so without this dedicated metadata check a
    /// FORCE-less migration would pass every other test trivially while
    /// providing zero real protection.
    #[tokio::test]
    async fn rls_actually_forces_for_table_owner_not_just_enabled() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        for table in [
            "bookings",
            "accept_rules",
            "portal_users",
            "agency_credentials",
        ] {
            let (forced,): (bool,) =
                sqlx::query_as("SELECT relforcerowsecurity FROM pg_class WHERE relname = $1")
                    .bind(table)
                    .fetch_one(&pool)
                    .await
                    .unwrap_or_else(|e| panic!("checking relforcerowsecurity for {table}: {e}"));
            assert!(
                forced,
                "{table} must have FORCE ROW LEVEL SECURITY set, not just ENABLE"
            );
        }
    }

    /// `tenants` and `archive_runs` are deliberately excluded from RLS
    /// (`tenants` has no `tenant_id` column to key a policy on;
    /// `archive_runs` is a system-wide maintenance record, not tenant-scoped
    /// — see Task 6). Confirms the migration's exclusion list wasn't
    /// accidentally widened or narrowed.
    #[tokio::test]
    async fn rls_excludes_tenants_and_archive_runs() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        for table in ["tenants", "archive_runs"] {
            let (enabled, forced): (bool, bool) = sqlx::query_as(
                "SELECT relrowsecurity, relforcerowsecurity FROM pg_class WHERE relname = $1",
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|e| panic!("checking relrowsecurity for {table}: {e}"));
            assert!(!enabled, "{table} must NOT have RLS enabled");
            assert!(!forced, "{table} must NOT have RLS forced");
        }
    }

    // -- Fase 6a Task 2: tenants / portal_users / portal_sessions queries --

    #[tokio::test]
    async fn tenants_find_by_slug_finds_seeded_and_none_for_unknown() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;
        let slug = format!("test-{tenant_id}");

        let found = tenants::find_by_slug(&pool, &slug)
            .await
            .expect("find_by_slug query")
            .expect("seeded tenant must be found");
        assert_eq!(found.id, tenant_id);
        assert_eq!(found.slug, slug);

        let missing = tenants::find_by_slug(&pool, "no-such-tenant-slug-at-all")
            .await
            .expect("find_by_slug query for unknown slug");
        assert!(missing.is_none(), "unknown slug must return None");

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// `tenants` has no RLS policy (`rls_excludes_tenants_and_archive_runs`),
    /// but until migration `0017_tenants_app_role_grant.sql` `app_role` had
    /// no GRANT on it at all — a lookup equivalent to `find_by_slug` would
    /// have failed with `permission denied for table tenants` the moment
    /// Fase 6a Task 9 switches the production pool to `app_role`. Proves
    /// that gap is actually closed, under the exact role / no-tenant-context
    /// conditions `find_by_slug` runs under for real (this is what Task 2's
    /// required RLS investigation resolved for `tenants`).
    ///
    /// Exercised on one acquired connection with `SET ROLE app_role`
    /// directly (same reasoning as `app_role_tenant_tx`'s doc comment) —
    /// `find_by_slug` itself takes `&PgPool`, which hands out an arbitrary
    /// pooled connection per call, so it cannot be pinned to one
    /// role-switched connection from a test; this runs the identical SQL
    /// shape `find_by_slug` executes instead.
    #[tokio::test]
    async fn tenants_lookup_works_for_app_role_with_no_tenant_context() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;
        let slug = format!("test-{tenant_id}");

        let mut conn = pool.acquire().await.expect("acquire");
        sqlx::query("SET ROLE app_role")
            .execute(&mut *conn)
            .await
            .expect("set role app_role");

        let found: Option<models::Tenant> =
            sqlx::query_as("SELECT id, name, slug, created_at FROM tenants WHERE slug = $1")
                .bind(&slug)
                .fetch_optional(&mut *conn)
                .await
                .expect("find_by_slug-equivalent query as app_role");
        assert!(
            found.is_some(),
            "app_role must be able to read tenants with no app.tenant_id set at all"
        );

        sqlx::query("RESET ROLE").execute(&mut *conn).await.ok();
        drop(conn);

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Seeds the SAME username under two different tenants and proves
    /// `find_by_username` only ever returns the caller's own tenant's row —
    /// proves the query's tenant filter actually isolates, not just "the
    /// query runs and returns something."
    #[tokio::test]
    async fn portal_users_find_by_username_isolates_by_tenant() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        let tenant_a = insert_test_tenant(&pool).await;
        let tenant_b = insert_test_tenant(&pool).await;

        let user_a: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO portal_users (tenant_id, username, password_hash, display_name)
             VALUES ($1, 'shared-username', 'hash-a', 'Tenant A User') RETURNING id",
        )
        .bind(tenant_a)
        .fetch_one(&pool)
        .await
        .expect("insert tenant a user");

        let user_b: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO portal_users (tenant_id, username, password_hash, display_name)
             VALUES ($1, 'shared-username', 'hash-b', 'Tenant B User') RETURNING id",
        )
        .bind(tenant_b)
        .fetch_one(&pool)
        .await
        .expect("insert tenant b user");

        let found_a = portal_users::find_by_username(&pool, tenant_a, "shared-username")
            .await
            .expect("find_by_username tenant a")
            .expect("tenant a must find its own user");
        assert_eq!(found_a.id, user_a.0);
        assert_eq!(found_a.password_hash, "hash-a");

        let found_b = portal_users::find_by_username(&pool, tenant_b, "shared-username")
            .await
            .expect("find_by_username tenant b")
            .expect("tenant b must find its own user");
        assert_eq!(found_b.id, user_b.0);
        assert_eq!(found_b.password_hash, "hash-b");

        let cross = portal_users::find_by_username(&pool, tenant_a, "nonexistent-username")
            .await
            .expect("find_by_username unknown username");
        assert!(cross.is_none());

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_a)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_b)
            .execute(&pool)
            .await
            .ok();
    }

    /// `agency_credentials::list_all` (Fase 6a Task 9, the account-bootstrap
    /// loop's own query): returns every row for the given tenant, isolates
    /// by tenant like every other `begin_tenant_tx`-backed lookup in this
    /// module, and returns an empty `Vec` (not an error) for a tenant with
    /// zero credentials.
    #[tokio::test]
    async fn agency_credentials_list_all_isolates_by_tenant() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        let tenant_a = insert_test_tenant(&pool).await;
        let tenant_b = insert_test_tenant(&pool).await;

        for (label, username) in [("primary", "agent-a-1"), ("secondary", "agent-a-2")] {
            sqlx::query(
                "INSERT INTO agency_credentials \
                 (tenant_id, label, username, ciphertext, nonce, key_version) \
                 VALUES ($1, $2, $3, $4, $5, 1)",
            )
            .bind(tenant_a)
            .bind(label)
            .bind(username)
            .bind(vec![0xAA_u8, 0xBB])
            .bind(vec![0u8; 12])
            .execute(&pool)
            .await
            .expect("insert tenant a credential");
        }
        sqlx::query(
            "INSERT INTO agency_credentials \
             (tenant_id, label, username, ciphertext, nonce, key_version) \
             VALUES ($1, 'primary', 'agent-b-1', $2, $3, 1)",
        )
        .bind(tenant_b)
        .bind(vec![0xCC_u8, 0xDD])
        .bind(vec![1u8; 12])
        .execute(&pool)
        .await
        .expect("insert tenant b credential");

        let rows_a = agency_credentials::list_all(&pool, tenant_a)
            .await
            .expect("list_all tenant a");
        assert_eq!(rows_a.len(), 2, "tenant a must see exactly its own 2 rows");
        assert!(rows_a.iter().all(|r| r.tenant_id == tenant_a));
        assert!(rows_a.iter().any(|r| r.username == "agent-a-1"));
        assert!(rows_a.iter().any(|r| r.username == "agent-a-2"));
        assert!(
            rows_a.iter().all(|r| r.username != "agent-b-1"),
            "tenant a must not see tenant b's row"
        );

        let rows_b = agency_credentials::list_all(&pool, tenant_b)
            .await
            .expect("list_all tenant b");
        assert_eq!(rows_b.len(), 1);
        assert_eq!(rows_b[0].username, "agent-b-1");

        let tenant_c = insert_test_tenant(&pool).await;
        let rows_c = agency_credentials::list_all(&pool, tenant_c)
            .await
            .expect("list_all tenant c");
        assert!(
            rows_c.is_empty(),
            "a tenant with zero agency_credentials rows must get an empty Vec, not an error"
        );

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_a)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_b)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_c)
            .execute(&pool)
            .await
            .ok();
    }

    /// `find_by_id` (added Fase 6a Task 3 for the session-auth middleware,
    /// which only has `portal_user_id` — not a username — to look up from a
    /// validated session row). Same tenant-isolation shape as
    /// `portal_users_find_by_username_isolates_by_tenant` above: tenant A's
    /// context must not find tenant B's row by id even if it somehow had the
    /// id, and an unknown id is a clean `None`, not an error.
    #[tokio::test]
    async fn portal_users_find_by_id_isolates_by_tenant() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");

        let tenant_a = insert_test_tenant(&pool).await;
        let tenant_b = insert_test_tenant(&pool).await;

        let user_a: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO portal_users (tenant_id, username, password_hash, display_name)
             VALUES ($1, 'find-by-id-a', 'hash-a', 'Tenant A User') RETURNING id",
        )
        .bind(tenant_a)
        .fetch_one(&pool)
        .await
        .expect("insert tenant a user");

        let found_a = portal_users::find_by_id(&pool, tenant_a, user_a.0)
            .await
            .expect("find_by_id tenant a")
            .expect("tenant a must find its own user by id");
        assert_eq!(found_a.username, "find-by-id-a");

        let cross = portal_users::find_by_id(&pool, tenant_b, user_a.0)
            .await
            .expect("find_by_id cross-tenant lookup");
        assert!(
            cross.is_none(),
            "tenant B must not find tenant A's user by id"
        );

        let unknown = portal_users::find_by_id(&pool, tenant_a, uuid::Uuid::new_v4())
            .await
            .expect("find_by_id unknown id");
        assert!(unknown.is_none());

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_a)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_b)
            .execute(&pool)
            .await
            .ok();
    }

    /// Round-trip proof for `portal_sessions::{create, find_valid_by_hash,
    /// delete}`: a freshly created session is found by its hash; an EXPIRED
    /// session (created with a negative TTL) is NOT found even though the
    /// row genuinely exists; and after `delete`, the (still valid) session
    /// is no longer found.
    #[tokio::test]
    async fn portal_session_create_find_valid_by_hash_delete_round_trip() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let portal_user_id: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO portal_users (tenant_id, username, password_hash, display_name)
             VALUES ($1, 'session-owner', 'hash', 'Session Owner') RETURNING id",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("insert portal_user");

        let valid_hash = [7u8; 32];
        let created = portal_sessions::create(
            &pool,
            tenant_id,
            portal_user_id.0,
            valid_hash,
            Some("203.0.113.9"),
            Some("test-agent"),
            chrono::Duration::hours(1),
        )
        .await
        .expect("create session");
        assert_eq!(created.tenant_id, tenant_id);
        assert_eq!(created.portal_user_id, portal_user_id.0);

        let found = portal_sessions::find_valid_by_hash(&pool, valid_hash)
            .await
            .expect("find_valid_by_hash query")
            .expect("just-created, non-expired session must be found");
        assert_eq!(found.id, created.id);

        // Expired session (negative TTL) must NOT be found, even though the
        // row genuinely exists.
        let expired_hash = [8u8; 32];
        let expired = portal_sessions::create(
            &pool,
            tenant_id,
            portal_user_id.0,
            expired_hash,
            None,
            None,
            chrono::Duration::seconds(-1),
        )
        .await
        .expect("create expired session");
        let row_exists: (i64,) =
            sqlx::query_as("SELECT count(*) FROM portal_sessions WHERE id = $1")
                .bind(expired.id)
                .fetch_one(&pool)
                .await
                .expect("count expired row");
        assert_eq!(row_exists.0, 1, "expired session row must actually exist");
        let expired_lookup = portal_sessions::find_valid_by_hash(&pool, expired_hash)
            .await
            .expect("find_valid_by_hash query for expired session");
        assert!(
            expired_lookup.is_none(),
            "expired session must not be found by find_valid_by_hash"
        );

        // delete then find_valid_by_hash returns None.
        portal_sessions::delete(&pool, tenant_id, created.id)
            .await
            .expect("delete session");
        let after_delete = portal_sessions::find_valid_by_hash(&pool, valid_hash)
            .await
            .expect("find_valid_by_hash query after delete");
        assert!(
            after_delete.is_none(),
            "deleted session must not be found by find_valid_by_hash"
        );

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    #[tokio::test]
    async fn portal_sessions_touch_last_seen_advances_timestamp() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let portal_user_id: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO portal_users (tenant_id, username, password_hash, display_name)
             VALUES ($1, 'touch-owner', 'hash', 'Touch Owner') RETURNING id",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("insert portal_user");

        let created = portal_sessions::create(
            &pool,
            tenant_id,
            portal_user_id.0,
            [9u8; 32],
            None,
            None,
            chrono::Duration::hours(1),
        )
        .await
        .expect("create session");

        // Backdate rather than sleeping, so the assertion is unambiguous
        // regardless of clock granularity.
        sqlx::query(
            "UPDATE portal_sessions SET last_seen_at = now() - interval '1 hour' WHERE id = $1",
        )
        .bind(created.id)
        .execute(&pool)
        .await
        .expect("backdate last_seen_at");

        portal_sessions::touch_last_seen(&pool, tenant_id, created.id)
            .await
            .expect("touch_last_seen");

        let refetched: models::PortalSession =
            sqlx::query_as("SELECT * FROM portal_sessions WHERE id = $1")
                .bind(created.id)
                .fetch_one(&pool)
                .await
                .expect("refetch session");
        assert!(
            refetched.last_seen_at > created.last_seen_at,
            "touch_last_seen must advance last_seen_at forward from its backdated value"
        );

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }

    /// Proves the RLS carve-out added in migration
    /// `0018_portal_sessions_lookup_by_hash_fn.sql`: `portal_sessions` IS
    /// RLS-protected (unlike `tenants`), so once Fase 6a Task 9 switches the
    /// production pool to `app_role`, a plain `SELECT ... WHERE
    /// token_hash = $1` against the base table would see zero rows with no
    /// `app.tenant_id` set — silently breaking every login. The
    /// `portal_sessions_find_valid_by_hash` SQL function is `SECURITY
    /// DEFINER` specifically so it keeps working for `app_role` under
    /// exactly those conditions.
    ///
    /// Exercised via `app_role` (same reasoning as `app_role_tenant_tx`'s
    /// doc comment: `tower` is a superuser and bypasses RLS regardless, so a
    /// version of this test against the raw pool would prove nothing about
    /// the carve-out actually being necessary or working). Includes a
    /// sanity control: a plain `SELECT` against the base table, same role,
    /// same missing tenant context, must be blocked — otherwise this test
    /// would pass even if RLS on `portal_sessions` were silently disabled.
    #[tokio::test]
    async fn portal_sessions_find_valid_by_hash_fn_works_for_app_role_with_no_tenant_context() {
        let pool = connect(&test_database_url()).await.expect("connect");
        run_migrations(&pool).await.expect("migrate");
        let tenant_id = insert_test_tenant(&pool).await;

        let portal_user_id: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO portal_users (tenant_id, username, password_hash, display_name)
             VALUES ($1, 'app-role-session-owner', 'hash', 'App Role Session Owner') RETURNING id",
        )
        .bind(tenant_id)
        .fetch_one(&pool)
        .await
        .expect("insert portal_user");

        let hash = [42u8; 32];
        let created = portal_sessions::create(
            &pool,
            tenant_id,
            portal_user_id.0,
            hash,
            None,
            None,
            chrono::Duration::hours(1),
        )
        .await
        .expect("create session");

        let mut conn = pool.acquire().await.expect("acquire");
        sqlx::query("SET ROLE app_role")
            .execute(&mut *conn)
            .await
            .expect("set role app_role");

        // Sanity control: a plain SELECT against the base table, as
        // app_role, with NO app.tenant_id set, must see nothing — proving
        // RLS really does apply to portal_sessions for app_role (and that
        // the carve-out below is doing real work, not papering over a
        // no-op).
        let blocked: Vec<(uuid::Uuid,)> =
            sqlx::query_as("SELECT id FROM portal_sessions WHERE token_hash = $1")
                .bind(hash.as_slice())
                .fetch_all(&mut *conn)
                .await
                .expect("plain select as app_role");
        assert_eq!(
            blocked.len(),
            0,
            "a plain SELECT against the base table must be blocked by RLS for app_role with no tenant context"
        );

        // The carve-out function, called the exact same way, must find it.
        let via_fn: Option<models::PortalSession> =
            sqlx::query_as("SELECT * FROM portal_sessions_find_valid_by_hash($1)")
                .bind(hash.as_slice())
                .fetch_optional(&mut *conn)
                .await
                .expect("carve-out function as app_role");
        assert_eq!(
            via_fn.map(|s| s.id),
            Some(created.id),
            "SECURITY DEFINER carve-out must find the session for app_role with no tenant context"
        );

        sqlx::query("RESET ROLE").execute(&mut *conn).await.ok();
        drop(conn);

        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&pool)
            .await
            .ok();
    }
}
