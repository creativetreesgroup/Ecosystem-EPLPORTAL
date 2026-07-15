-- Backend/crates/store/migrations/0020_bookings_account_id.sql
-- A `bookings` row has never recorded WHICH SPX account/login session saw
-- it — only `(tenant_id, spx_id)`. This was fine while nothing needed to
-- dispatch an HTTP call on a specific account's behalf from a `bookings`
-- row alone, but Fase 6c's manual-accept route does exactly that
-- (`executor::try_claim_manual(account_id, spx_id, dedup)` needs a real
-- `account_id`). The executor's own claim keys are already account-scoped
-- (`spx:claim:<account_id>:<spx_id>`), which only makes sense if the same
-- `spx_id` can legitimately be visible to more than one sibling account
-- under a tenant — meaning the OLD `(tenant_id, spx_id)` uniqueness was
-- already a latent collision risk, not just a missing convenience column.
ALTER TABLE bookings ADD COLUMN account_id TEXT NOT NULL DEFAULT '';

ALTER TABLE bookings DROP CONSTRAINT bookings_tenant_spx_id_unique;
ALTER TABLE bookings ADD CONSTRAINT bookings_tenant_account_spx_id_unique
  UNIQUE (tenant_id, account_id, spx_id);
