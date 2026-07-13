// Backend/crates/spx-client/tests/agency_credentials_pg.rs
//! Real Postgres round-trip: encrypt an SPX password, insert into Fase 2's
//! `agency_credentials`, fetch, decrypt, assert equality — and assert the stored
//! ciphertext never contains the plaintext. Connects to the Fase 2 tower-postgres
//! container at 127.0.0.1:15432. Run with `-- --test-threads=1`.
use spx_client::crypto::envelope::{
    decrypt_agency_password, encrypt_agency_password, MasterKey, KEY_VERSION,
};
use uuid::Uuid;

fn test_database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

fn test_master() -> MasterKey {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).unwrap();
    MasterKey::from_bytes(b)
}

#[tokio::test]
async fn agency_credential_encrypt_store_fetch_decrypt() {
    let pool = store::connect(&test_database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");

    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("SPX Test Tenant")
        .bind(format!("spx-{tenant_id}"))
        .execute(&pool)
        .await
        .expect("insert tenant");

    let master = test_master();
    let plaintext_password = "sup3r-s3cret-spx-pw";
    let ct = encrypt_agency_password(&master, tenant_id, plaintext_password).expect("encrypt");

    // Insert under RLS via the tenant-scoped transaction (Fase 2 pattern).
    let mut tx = store::begin_tenant_tx(&pool, tenant_id).await.expect("tx");
    sqlx::query(
        "INSERT INTO agency_credentials \
         (tenant_id, label, username, ciphertext, nonce, key_version) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(tenant_id)
    .bind("primary")
    .bind("agency-login@example.com") // username stays plaintext (surface-safe)
    .bind(&ct.bytes)
    .bind(&ct.nonce[..])
    .bind(KEY_VERSION)
    .execute(&mut *tx)
    .await
    .expect("insert credential");
    tx.commit().await.expect("commit");

    // Fetch back and decrypt.
    let row: store::models::AgencyCredential = {
        let mut tx = store::begin_tenant_tx(&pool, tenant_id).await.expect("tx2");
        let r = sqlx::query_as::<_, store::models::AgencyCredential>(
            "SELECT * FROM agency_credentials WHERE tenant_id = $1 AND label = 'primary'",
        )
        .bind(tenant_id)
        .fetch_one(&mut *tx)
        .await
        .expect("fetch credential");
        tx.commit().await.ok();
        r
    };

    assert_eq!(row.key_version, KEY_VERSION);
    // The stored ciphertext must NEVER contain the plaintext bytes.
    assert!(
        !row.ciphertext.windows(plaintext_password.len()).any(|w| w == plaintext_password.as_bytes()),
        "plaintext password leaked into stored ciphertext"
    );

    let nonce: [u8; 12] = row.nonce.as_slice().try_into().expect("12-byte nonce");
    let decrypted = decrypt_agency_password(&master, tenant_id, &row.ciphertext, &nonce)
        .expect("decrypt");
    use spx_client::crypto::secret::ExposeSecret;
    assert_eq!(decrypted.expose_secret(), plaintext_password);

    // Cleanup.
    sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(&pool).await.ok();
}

/// AAD-binding property: a row encrypted for tenant A must fail to decrypt
/// under tenant B's AAD, not silently succeed with garbage or leak the real
/// plaintext. Complements the pure-crypto `wrong_aad_fails` unit test in
/// `crypto::envelope` by proving the same property against a row that has
/// actually round-tripped through Postgres (real BYTEA encode/decode of
/// `ciphertext`/`nonce`, not just in-memory bytes).
#[tokio::test]
async fn agency_credential_wrong_tenant_fails_to_decrypt() {
    let pool = store::connect(&test_database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    for (id, label) in [(tenant_a, "Tenant A"), (tenant_b, "Tenant B")] {
        sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(label)
            .bind(format!("spx-{id}"))
            .execute(&pool)
            .await
            .expect("insert tenant");
    }

    let master = test_master();
    let plaintext_password = "another-s3cret-spx-pw";
    let ct = encrypt_agency_password(&master, tenant_a, plaintext_password).expect("encrypt");

    let mut tx = store::begin_tenant_tx(&pool, tenant_a).await.expect("tx");
    sqlx::query(
        "INSERT INTO agency_credentials \
         (tenant_id, label, username, ciphertext, nonce, key_version) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(tenant_a)
    .bind("primary")
    .bind("agency-a@example.com")
    .bind(&ct.bytes)
    .bind(&ct.nonce[..])
    .bind(KEY_VERSION)
    .execute(&mut *tx)
    .await
    .expect("insert credential");
    tx.commit().await.expect("commit");

    let row: store::models::AgencyCredential = {
        let mut tx = store::begin_tenant_tx(&pool, tenant_a).await.expect("tx2");
        let r = sqlx::query_as::<_, store::models::AgencyCredential>(
            "SELECT * FROM agency_credentials WHERE tenant_id = $1 AND label = 'primary'",
        )
        .bind(tenant_a)
        .fetch_one(&mut *tx)
        .await
        .expect("fetch credential");
        tx.commit().await.ok();
        r
    };

    let nonce: [u8; 12] = row.nonce.as_slice().try_into().expect("12-byte nonce");

    // Decrypting the row that was encrypted for tenant A, but using tenant
    // B's AAD, must fail outright (AES-GCM auth tag mismatch) — not succeed
    // with garbage plaintext and not return the real plaintext.
    let wrong_tenant_result = decrypt_agency_password(&master, tenant_b, &row.ciphertext, &nonce);
    assert!(
        wrong_tenant_result.is_err(),
        "decrypting a tenant-A row under tenant-B's AAD must fail, not succeed"
    );

    // Control: the correct tenant_id still decrypts fine, proving the
    // failure above is specifically about the AAD/tenant mismatch.
    use spx_client::crypto::secret::ExposeSecret;
    let correct = decrypt_agency_password(&master, tenant_a, &row.ciphertext, &nonce)
        .expect("decrypt with correct tenant_id must still succeed");
    assert_eq!(correct.expose_secret(), plaintext_password);

    // Cleanup.
    sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_a).execute(&pool).await.ok();
    sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_b).execute(&pool).await.ok();
}
