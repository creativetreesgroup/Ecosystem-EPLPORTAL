# Fase 7g: `/settings` shell + `/settings/branding` — Design

**Status:** Approved for implementation.

## Context

The master spec (`Docs/tower-master-spec.md:183`) names `/settings` as one of Fase 7's 7 surfaces, with no content-level detail beyond that. `TopNav.svelte` has carried a `/settings` nav entry (404ing until built) since Fase 7b, the same disclosed-gap pattern `/command` itself had before its own build.

Unlike every other Fase 7 page, `/settings` isn't one backend resource — it's **five independent, already-built, never-consumed CRUD surfaces**, all shipped backend-only since Fase 6b/6d: Branding (`GET/PUT /branding`), Bot/WAHA config (`GET/PUT /bot/settings`), Locations (`GET/POST/DELETE /locations`), Sub-users (`GET/POST/DELETE /auth/portal-users`), and SPX Credentials (`GET/PUT/DELETE /auth/spx-credentials`). (`GET/DELETE /bot/logs` is a sixth surface under `/bot`, but it's already fully consumed by Fase 7f's `/activity` page — out of scope here.)

A brainstorming session (2026-07-19) resolved the resulting scope/navigation questions:
- **Decomposition:** five separate sub-phases, one resource per phase (7g-7k), each its own full spec→plan→SDD→review→merge cycle — matching this project's established granularity (7c-7f each shipped one resource or a tightly related pair).
- **Navigation:** one `/settings` nav entry, five sub-routes underneath (`/settings/branding`, `/settings/bot`, `/settings/locations`, `/settings/sub-users`, `/settings/credentials`), tied together by a shared secondary-nav shell.
- **Order:** Branding first — simplest surface (one singleton form, no list-CRUD, no especially sensitive data), and the sub-phase that builds it also builds the shared shell every later sub-phase reuses.

This doc covers **only 7g**: the shell plus the Branding page.

## Scope

**In scope:**
- `Frontend/src/routes/(app)/settings/+layout.svelte` — the shared shell all five sub-phases will live under: a secondary nav distinct from `TopNav`, currently listing exactly one entry ("Branding").
- `/settings/branding` — view/edit `title`, `subtitle`, `site_name`, `brand_tag`, `logo_data_uri`, `favicon_data_uri` via the existing `GET/PUT /branding` endpoint. Logo/favicon are upload-with-preview-and-remove, not raw base64 text entry.
- Read-only view for non-main-account sessions (the established `<fieldset disabled>` pattern from `/rules`/`/price`).

**Out of scope (deliberately deferred, not silently dropped):**
- **Bot/WAHA config, Locations, Sub-users, SPX Credentials** — each gets its own later sub-phase (7h-7k). No placeholder "coming soon" nav entries for them either: matches this project's own established convention (see `TopNav.svelte`'s own header comment) of not building UI for surfaces that don't exist yet. The shell's nav array simply grows by one entry per future sub-phase.
- **Any actual consumption of branding data elsewhere.** `GET /branding` is public+rate-limited in the backend specifically because the reference app used it to brand an external, unauthenticated public price page — **no such page exists in this Frontend**, and whether TOWER ever builds one is an unraised, separate scope question. This phase also does NOT wire `site_name`/`logo_data_uri` into the portal's own chrome (`TopNav`'s hardcoded "TOWER" text, `app.html`'s title/favicon) — that's a distinct decision this phase doesn't assume either way.

## Backend

No changes. Existing surface this sub-phase depends on (`Backend/crates/api-gateway/src/routes/branding.rs`, mounted via a separately body-limit-layered sub-router per `lib.rs::build_router`'s own doc comment — the 15MB carve-out already exists):

- **`GET /branding`** — public, no session required, rate-limited. Returns `Branding` (200, even when nothing has ever been saved — falls back to `Branding::default()`, never 404).
- **`PUT /branding`** — `session_auth` + `Permission::ManageBranding` (main-account only). Body: `BrandingInput` (all fields optional/defaulted). Returns the normalized, persisted `Branding`.
- **`Branding` shape:** `{title: string, subtitle: string, site_name: string, brand_tag: string, logo_data_uri: string|null, favicon_data_uri: string|null}`.
- **Server-side validation** (`branding.rs::validate_and_normalize`, must be mirrored client-side — see below): `title` required, trimmed, ≤60 chars. `subtitle` ≤160 chars (empty allowed). `site_name` ≤60 chars; if submitted blank, silently falls back to the default site name rather than erroring. `brand_tag` ≤20 chars. `logo_data_uri`/`favicon_data_uri`, if present, must match `data:image/(png|jpeg|webp);base64,...` exactly (SVG/ICO rejected — SVG can carry executable script) and decode to ≤5MB each; an empty string is treated as "no image" (cleared), not a validation error.

## Frontend Architecture

**Files:**
- `Frontend/src/routes/(app)/settings/+layout.svelte` — secondary nav shell (one link: Branding).
- `Frontend/src/routes/(app)/settings/branding/+page.svelte` — page assembly: form fields, file pickers with preview, save, read-only gating.
- `Frontend/src/lib/branding.ts` — pure logic, unit-tested (TDD): validation functions mirroring every backend rule above (so client-side rejection matches server-side rejection exactly, no drift), and a `fileToDataUri(file): Promise<string>` helper wrapping `FileReader.readAsDataURL`.
- `Frontend/src/lib/api-branding.ts` — typed REST helpers: `fetchBranding(): Promise<Branding>`, `saveBranding(input: BrandingInput): Promise<Branding>`.

**Data flow:**
- On mount, `+page.svelte` calls `fetchBranding()` and populates local form state (this is always a 200 with real or default values — no empty/loading-forever state to design around).
- Logo/favicon inputs: `<input type="file" accept="image/png,image/jpeg,image/webp">`. The `accept` attribute only narrows the OS file picker — it is not validation (a renamed or drag-dropped file can bypass it) — so every selected file still goes through `branding.ts`'s own type/size checks before ever being read or sent. A file that fails validation shows an inline error and never reaches `fileToDataUri`/the network. A file that passes gets read to a data URI, shown immediately in an `<img>` preview, and held in local state until Save.
- An explicit **Remove** button per image sets that field to `null` in local state (distinct from "untouched" — a save after Remove must actually send `null` so a previously-saved image is truly cleared, not silently kept because the field was omitted).
- **Save** calls `saveBranding(...)` with the full current form state; on success, the form re-populates from the response (not from what was submitted), so it reflects exactly what the backend normalized and persisted (e.g., a blank `site_name` submitted becomes the fallback default in the response — showing that, not the blank the user typed, avoids the form silently lying about what's actually saved).

**RBAC:** `data.user.is_main_account` (same convention as `/rules`/`/price`/`/activity`) wraps the entire form in a disabled `<fieldset>` for non-main-account sessions — view-only, matching the established pattern. No content-gating is needed here (unlike `/activity`'s Log Bot tab): `GET /branding` has no permission gate at all, so every authenticated session, main or not, can see the current values; only `PUT` is gated.

**Accessibility:** consistent with 7a-7f's bar — tokens-only styling, every input labeled, 44px tap targets, focus-visible rings, `aria-live="polite"` region for save success/error, image previews get real descriptive `alt` text (not decorative/empty), file inputs remain keyboard-operable (native file input, no custom drag-drop widget — nothing else in this codebase has one, and a plain input fully covers the requirement).

## Testing

- **Unit (Vitest):** `branding.ts` — every validation rule above (title required/too long, subtitle/site_name/brand_tag length boundaries, site_name-blank-falls-back is a save-time backend behavior so client-side only needs to allow submitting blank, not reject it; data-URI MIME allowlist, oversized rejection, malformed base64 rejection). `api-branding.ts` — wire mapping, `PUT` body shape matches `BrandingInput` exactly.
- **E2E (Playwright), `Frontend/tests/settings-branding.spec.ts`:** unauthenticated visit to `/settings` (and `/settings/branding` directly) redirects to `/login`; main-account loads the page (seeded or default values), edits title/site_name, uploads a small real PNG fixture as logo, saves, reloads, confirms every field including the logo persisted; non-main-account session sees a disabled read-only form; selecting an oversized or wrong-type file shows an inline error and fires no network request (verified via Playwright's request-interception/route assertion, not just a UI check).

## Open Questions for the Implementer

None — this design resolves every scope question raised during brainstorming. Any genuinely new ambiguity found during implementation should be raised through the normal task-brief escalation path.
