// Frontend/src/lib/api-locations.ts
// Typed REST layer for /settings/locations. Deliberately a SEPARATE module from
// Frontend/src/lib/api-rules.ts's own fetchLocations/createLocation (which serves /rules' inline
// LocationCombobox create-flow) — duplicating these 3 small functions is cheaper than introducing
// a cross-page dependency, matching this codebase's tolerance for small, page-scoped API modules
// (api-prices.ts doesn't import from api-rules.ts either). Wire shape verified against
// Backend/crates/api-gateway/src/routes/locations.rs — id/name already match camelCase
// field-for-field, no snake_case conversion needed.
import { apiPost, ApiError } from './api';

export type LocationItem = {
	id: string;
	name: string;
};

export async function fetchLocations(): Promise<LocationItem[]> {
	const res = await fetch('/locations', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch locations');
	return res.json();
}

export async function createLocation(name: string): Promise<LocationItem> {
	return apiPost<LocationItem>('/locations', { name });
}

/** `DELETE /locations/{id}` returns `204 No Content` on success — never call `res.json()` on
 * this response, there is no body to parse. */
export async function deleteLocation(id: string): Promise<void> {
	const res = await fetch(`/locations/${id}`, { method: 'DELETE', credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to delete location');
}
