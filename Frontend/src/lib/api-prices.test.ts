// Frontend/src/lib/api-prices.test.ts
// No network for the pure mapping — fetchPrices/createPrice/updatePrice/deletePrice themselves
// are exercised for real by Frontend/tests/price.spec.ts (Task 6) against a live backend, PLUS a
// vi.stubGlobal('fetch', ...) regression guard here for the one load-bearing HTTP-method detail
// this module has (PUT for update, DELETE for delete — neither of which apiPost can send), same
// precedent as api-rules.test.ts's saveSettings guard (Fase 7d, added after a review finding
// that the brief's own "no test needed" reasoning cited a false precedent).
import { describe, it, expect, vi, afterEach } from 'vitest';
import { priceOutputToDraft, draftToPriceInput, fetchPrices, updatePrice, deletePrice } from './api-prices';
import { newPriceDraft } from './prices';

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('priceOutputToDraft', () => {
	it('maps every snake_case field to its camelCase PriceDraft equivalent', () => {
		const wire = {
			id: 'server-uuid-1',
			route_code: 'JKT-BDG-01',
			region: 'Jawa',
			origin: 'Jakarta DC',
			destinations: ['Bandung DC'],
			price: 150000,
			vehicle_type: 'TRONTON'
		};
		const draft = priceOutputToDraft(wire);
		expect(draft.id).toBe('server-uuid-1');
		expect(draft.clientKey).toMatch(/^[0-9a-f-]{36}$/);
		expect(draft.routeCode).toBe('JKT-BDG-01');
		expect(draft.region).toBe('Jawa');
		expect(draft.origin).toBe('Jakarta DC');
		expect(draft.destinations).toEqual(['Bandung DC']);
		expect(draft.price).toBe(150000);
		expect(draft.vehicleType).toBe('TRONTON');
	});
});

describe('draftToPriceInput', () => {
	it('maps every camelCase PriceDraft field to its snake_case wire equivalent, omitting id/clientKey', () => {
		const draft = {
			...newPriceDraft(),
			routeCode: 'JKT-BDG-01',
			region: 'Jawa',
			origin: 'Jakarta DC',
			destinations: ['Bandung DC'],
			price: 150000,
			vehicleType: 'TRONTON'
		};
		const wire = draftToPriceInput(draft);
		expect(wire).not.toHaveProperty('id');
		expect(wire).not.toHaveProperty('clientKey');
		expect(wire.route_code).toBe('JKT-BDG-01');
		expect(wire.region).toBe('Jawa');
		expect(wire.origin).toBe('Jakarta DC');
		expect(wire.destinations).toEqual(['Bandung DC']);
		expect(wire.price).toBe(150000);
		expect(wire.vehicle_type).toBe('TRONTON');
	});
});

describe('fetchPrices', () => {
	it('issues a GET to /prices', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify([]), { status: 200 });
			})
		);
		await fetchPrices();
		expect(calledUrl).toBe('/prices');
	});
});

describe('updatePrice', () => {
	it('issues a PUT to /prices/{id} with the mapped body', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(
					JSON.stringify({
						id: 'x',
						route_code: 'JKT-BDG-01',
						region: '',
						origin: 'Jakarta DC',
						destinations: ['Bandung DC'],
						price: 150000,
						vehicle_type: 'TRONTON'
					}),
					{ status: 200 }
				);
			})
		);
		const draft = { ...newPriceDraft(), routeCode: 'JKT-BDG-01', origin: 'Jakarta DC', destinations: ['Bandung DC'], price: 150000, vehicleType: 'TRONTON' };
		await updatePrice('server-id-1', draft);
		expect(calledUrl).toBe('/prices/server-id-1');
		expect(calledInit?.method).toBe('PUT');
		const body = JSON.parse(calledInit?.body as string);
		expect(body.route_code).toBe('JKT-BDG-01');
	});
});

describe('deletePrice', () => {
	it('issues a DELETE to /prices/{id} and does not attempt to parse a body', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(null, { status: 204 });
			})
		);
		await deletePrice('server-id-1');
		expect(calledUrl).toBe('/prices/server-id-1');
		expect(calledInit?.method).toBe('DELETE');
	});
});
