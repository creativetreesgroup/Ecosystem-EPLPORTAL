// Backend/crates/poller/tests/antidrift_pg.rs
//! DoD #7: (a) a partial sweep (fetch_complete=false) triggers NO expire/
//! resurrect — proven by constructing a FetchOutcome with fetch_complete=false
//! and asserting a live 'pending' row that is ABSENT from spx_id_set survives;
//! (b) a complete sweep (fetch_complete=true) DOES expire an absent pending row
//! and resurrect a wrongly-failed present row. Real Postgres @ 15432.
use std::collections::HashSet;

use poller::{run_anti_drift, FetchOutcome};
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

fn outcome(complete: bool, ids: &[&str]) -> FetchOutcome {
    FetchOutcome {
        fetch_complete: complete,
        spx_id_set: ids.iter().map(|s| s.to_string()).collect::<HashSet<_>>(),
        page_failures: 0,
        bookings: Vec::new(),
        was_full_sweep: complete,
    }
}

#[tokio::test]
async fn partial_sweep_never_expires_but_complete_sweep_does() {
    let pool = store::connect(&database_url()).await.unwrap();
    store::run_migrations(&pool).await.unwrap();

    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("AntiDrift")
        .bind(format!("ad-{tenant_id}"))
        .execute(&pool)
        .await
        .unwrap();

    // Seed: one live pending row ("LIVE") + one wrongly-failed row ("BACK").
    for (spx, status) in [("LIVE", "pending"), ("BACK", "failed")] {
        store::upsert_booking(
            &pool,
            tenant_id,
            &store::BookingUpsert {
                account_id: "test-account".to_string(),
                spx_id: spx.into(),
                status: status.into(),
                is_coc: false,
                raw_data: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    }
    // upsert can't set 'failed' directly (it inserts 'pending'); force BACK failed.
    sqlx::query("UPDATE bookings SET status='failed' WHERE tenant_id=$1 AND spx_id='BACK'")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .unwrap();

    // (a) Partial sweep whose id-set does NOT include LIVE → must NOT expire it.
    run_anti_drift(&pool, tenant_id, &outcome(false, &["OTHER"])).await.unwrap();
    let (live_status,): (String,) =
        sqlx::query_as("SELECT status FROM bookings WHERE tenant_id=$1 AND spx_id='LIVE'")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(live_status, "pending", "a partial sweep must NEVER expire a live ticket");

    // (b) Complete sweep that SEES BACK (resurrect) but NOT LIVE (expire).
    run_anti_drift(&pool, tenant_id, &outcome(true, &["BACK"])).await.unwrap();
    let (live2,): (String,) =
        sqlx::query_as("SELECT status FROM bookings WHERE tenant_id=$1 AND spx_id='LIVE'")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let (back2,): (String,) =
        sqlx::query_as("SELECT status FROM bookings WHERE tenant_id=$1 AND spx_id='BACK'")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(live2, "failed", "a complete sweep expires a pending ticket no longer present");
    assert_eq!(back2, "pending", "a complete sweep resurrects a wrongly-failed present ticket");

    sqlx::query("DELETE FROM tenants WHERE id=$1").bind(tenant_id).execute(&pool).await.ok();
}
