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
}
