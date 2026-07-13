// Backend/crates/executor/tests/quota_pg.rs
//! DoD #6: fire N concurrent apply_rule_consumption for the SAME rule (cap=2)
//! and assert exactly 2 succeed (accepted_count 1 then 2), the rest hit the cap,
//! the final persisted count is 2 (no lost update), and it never exceeds the
//! cap. Postgres @ 127.0.0.1:15432 + Redis @ 127.0.0.1:16379.
use executor::ExecutorHandle;
use store::QuotaConsumeOutcome;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}
fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_consume_never_exceeds_cap_no_lost_update() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let handle = std::sync::Arc::new(ExecutorHandle::connect(&redis_url()).await.expect("redis"));

    // Tenant + a capped rule (max=2, accepted=0).
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("Quota Tenant")
        .bind(format!("quota-{tenant_id}"))
        .execute(&pool)
        .await
        .expect("insert tenant");

    let rule_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO accept_rules (id, tenant_id, name, mode, max_accept_count, accepted_count) \
         VALUES ($1, $2, 'r', 'route', 2, 0)",
    )
    .bind(rule_id)
    .bind(tenant_id)
    .execute(&pool)
    .await
    .expect("insert rule");

    let account = format!("t{}", Uuid::new_v4().simple());

    // 8 concurrent attempts for the same rule.
    let mut tasks = Vec::new();
    for i in 0..8 {
        let h = handle.clone();
        let p = pool.clone();
        let acct = account.clone();
        tasks.push(tokio::spawn(async move {
            h.apply_rule_consumption(&p, tenant_id, &acct, rule_id, &format!("spx-{i}"))
                .await
                .expect("consume")
        }));
    }
    let mut consumed = 0usize;
    let mut cap_reached = 0usize;
    let mut seen_counts = Vec::new();
    for t in tasks {
        match t.await.unwrap() {
            QuotaConsumeOutcome::Consumed { accepted_count } => {
                consumed += 1;
                seen_counts.push(accepted_count);
            }
            QuotaConsumeOutcome::CapReached { .. } => cap_reached += 1,
            QuotaConsumeOutcome::NoRule => panic!("rule must exist"),
        }
    }
    assert_eq!(consumed, 2, "exactly cap (2) consumptions may succeed");
    assert_eq!(cap_reached, 6, "the other 6 must see the cap");
    seen_counts.sort_unstable();
    assert_eq!(
        seen_counts,
        vec![1, 2],
        "no lost update: counts are 1 and 2"
    );

    // Final persisted count must be exactly the cap — never exceeded.
    let (final_count,): (i32,) =
        sqlx::query_as("SELECT accepted_count FROM accept_rules WHERE id = $1")
            .bind(rule_id)
            .fetch_one(&pool)
            .await
            .expect("read final");
    assert_eq!(final_count, 2);

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
