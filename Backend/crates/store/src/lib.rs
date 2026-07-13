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
}
