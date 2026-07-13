pub mod models;
pub mod pool;

pub use pool::{begin_tenant_tx, connect, run_migrations};

#[cfg(test)]
mod tests {
    use super::*;

    fn test_database_url() -> String {
        std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:5432/tower".to_string())
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

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
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

        assert_eq!(first.route_signature.as_deref(), Some("padang dc|cileungsi dc|strict|all|"));

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
        assert_eq!(db_err.code().as_deref(), Some("23505"), "expected a unique_violation (23505), got: {db_err}");

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
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
        assert_eq!(tronton.route_signature.as_deref(), Some("padang dc|cileungsi dc|strict|all|tronton"));
        assert_eq!(fuso.route_signature.as_deref(), Some("padang dc|cileungsi dc|strict|all|fuso"));

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
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
            assert_eq!(row.0, *expected, "case {i}: spx_id={spx_id:?} booking_name={booking_name:?}");
        }

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
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
        sqlx::query("SET ROLE app_role").execute(&mut *conn).await.expect("set role");

        let update_result = sqlx::query("UPDATE accept_events SET outcome = 'rejected' WHERE id = $1")
            .bind(event_id.0)
            .execute(&mut *conn)
            .await;
        assert!(update_result.is_err(), "app_role must not be able to UPDATE accept_events");
        let update_err = update_result.unwrap_err();
        let update_db_err = update_err.as_database_error().expect("expected a database error");
        assert_eq!(
            update_db_err.code().as_deref(),
            Some("42501"),
            "expected insufficient_privilege (42501) on UPDATE, got: {update_db_err}"
        );

        let delete_result = sqlx::query("DELETE FROM accept_events WHERE id = $1")
            .bind(event_id.0)
            .execute(&mut *conn)
            .await;
        assert!(delete_result.is_err(), "app_role must not be able to DELETE accept_events");
        let delete_err = delete_result.unwrap_err();
        let delete_db_err = delete_err.as_database_error().expect("expected a database error");
        assert_eq!(
            delete_db_err.code().as_deref(),
            Some("42501"),
            "expected insufficient_privilege (42501) on DELETE, got: {delete_db_err}"
        );

        sqlx::query("RESET ROLE").execute(&mut *conn).await.ok();
        drop(conn);

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
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
        assert!(!row.auto_accept_enabled, "kill switch must default to false with zero explicit input");

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
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

        let fetched: models::Notification = sqlx::query_as("SELECT * FROM notifications WHERE id = $1")
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

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
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

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
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

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
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

        assert!(insert(serde_json::json!([]), "zero".into()).await.is_err(), "0 destinations must fail");
        assert!(
            insert(serde_json::json!(["A", "B", "C", "D", "E", "F"]), "six".into()).await.is_err(),
            "6 destinations must fail"
        );
        assert!(
            insert(serde_json::json!(["A", "B", "C"]), "three".into()).await.is_ok(),
            "3 destinations must succeed"
        );

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
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

        let dup_result = sqlx::query("INSERT INTO route_locations (tenant_id, name) VALUES ($1, $2)")
            .bind(tenant_id)
            .bind("Padang DC")
            .execute(&pool)
            .await;
        assert!(dup_result.is_err(), "duplicate (tenant_id, name) must violate the unique constraint");

        sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
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

        assert_eq!(inserted.status, "running", "status must default to 'running'");
        assert!(!inserted.dry_run, "dry_run must default to false");
        assert!(inserted.archive_path.is_none());
        assert!(inserted.sha256.is_none());

        let fetched: models::ArchiveRun = sqlx::query_as("SELECT * FROM archive_runs WHERE id = $1")
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
        assert!(bad_status.is_err(), "status must be constrained to running/completed/failed");

        sqlx::query("DELETE FROM archive_runs WHERE id = $1").bind(inserted.id).execute(&pool).await.ok();
    }
}
