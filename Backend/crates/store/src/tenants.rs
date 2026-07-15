// Backend/crates/store/src/tenants.rs
//! Deployment-tenant resolution. `find_by_slug` is the ONE query in this
//! crate that legitimately runs outside `begin_tenant_tx` — tenant
//! resolution happens BEFORE any tenant_id is known (there is nothing to
//! scope a `begin_tenant_tx` to yet).
//!
//! `tenants` carries no `tenant_id` column (it IS the tenant) and is
//! deliberately excluded from `migrations/0016_rls_policies.sql`'s RLS loop
//! — verified via `grep -n "tenants" migrations/0016_rls_policies.sql`
//! (only an explanatory comment matches, not the `ENABLE ROW LEVEL
//! SECURITY` array) and the `rls_excludes_tenants_and_archive_runs` test in
//! `lib.rs`. So a bare `pool` query needs no tenant context and is not
//! blocked by RLS.
//!
//! `app_role` still needs an ordinary table-level GRANT to read `tenants`
//! once `reactor-core`'s production pool switches to `app_role` (Fase 6a
//! Task 9) — added in migration `0017_tenants_app_role_grant.sql`.
use sqlx::PgPool;

use crate::models::Tenant;

pub async fn find_by_slug(pool: &PgPool, slug: &str) -> Result<Option<Tenant>, sqlx::Error> {
    sqlx::query_as::<_, Tenant>("SELECT id, name, slug, created_at FROM tenants WHERE slug = $1")
        .bind(slug)
        .fetch_optional(pool)
        .await
}
