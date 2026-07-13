// Backend/crates/spx-client/tests/waha_settings_pg.rs
//! Store the encrypted WAHA settings in `site_settings`, fetch the JSONB back,
//! decrypt, assert equality — and assert the stored JSONB column text never
//! contains the plaintext key (DoD #4c). Connects to tower-postgres @ :15432.
use spx_client::crypto::envelope::MasterKey;
use spx_client::waha_settings::{WahaSettings, SITE_SETTINGS_KEY};
use uuid::Uuid;

fn test_database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

fn master() -> MasterKey {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).unwrap();
    MasterKey::from_bytes(b)
}

#[tokio::test]
async fn waha_key_encrypted_in_site_settings_jsonb() {
    let pool = store::connect(&test_database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");

    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id).bind("WAHA Tenant").bind(format!("waha-{tenant_id}"))
        .execute(&pool).await.expect("insert tenant");

    let m = master();
    let api_key = "waha-secret-APIKEY-9988";
    let settings = WahaSettings::encrypt_new(&m, tenant_id, "http://waha:3000", "default", api_key)
        .expect("encrypt settings");

    let mut tx = store::begin_tenant_tx(&pool, tenant_id).await.expect("tx");
    sqlx::query("INSERT INTO site_settings (tenant_id, key, value) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(SITE_SETTINGS_KEY)
        .bind(settings.to_json_value())
        .execute(&mut *tx)
        .await
        .expect("insert site_settings");
    tx.commit().await.expect("commit");

    // Assert at the DB level that the JSONB text has no plaintext key.
    let (stored_text,): (String,) = sqlx::query_as(
        "SELECT value::text FROM site_settings WHERE tenant_id = $1 AND key = $2",
    )
    .bind(tenant_id)
    .bind(SITE_SETTINGS_KEY)
    .fetch_one(&pool)
    .await
    .expect("fetch jsonb text");
    assert!(!stored_text.contains(api_key), "plaintext WAHA key in stored JSONB: {stored_text}");

    // Fetch JSONB, decrypt, assert equality.
    let (value,): (serde_json::Value,) = sqlx::query_as(
        "SELECT value FROM site_settings WHERE tenant_id = $1 AND key = $2",
    )
    .bind(tenant_id)
    .bind(SITE_SETTINGS_KEY)
    .fetch_one(&pool)
    .await
    .expect("fetch jsonb");
    let parsed = WahaSettings::from_json_value(&value).expect("parse");
    use spx_client::crypto::secret::ExposeSecret;
    assert_eq!(parsed.decrypt_api_key(&m, tenant_id).unwrap().expose_secret(), api_key);
    assert_eq!(parsed.waha_url, "http://waha:3000");

    sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
}
