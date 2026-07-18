import { describe, it, expect } from 'vitest';
import { newPriceDraft, formatRupiah, matchesFilter, priceDraftIsValid, type PriceDraft } from './prices';

describe('newPriceDraft', () => {
	it('creates an empty draft with a fresh clientKey and no server id', () => {
		const draft = newPriceDraft();
		expect(draft.id).toBeNull();
		expect(draft.clientKey).toMatch(/^[0-9a-f-]{36}$/);
		expect(draft.routeCode).toBe('');
		expect(draft.region).toBe('');
		expect(draft.origin).toBe('');
		expect(draft.destinations).toEqual([]);
		expect(draft.price).toBe(0);
		expect(draft.vehicleType).toBe('');
	});

	it('two calls produce different clientKeys', () => {
		expect(newPriceDraft().clientKey).not.toBe(newPriceDraft().clientKey);
	});
});

describe('formatRupiah', () => {
	it('formats with Indonesian thousand separators and an Rp prefix', () => {
		expect(formatRupiah(1500000)).toBe('Rp 1.500.000');
	});

	it('formats zero', () => {
		expect(formatRupiah(0)).toBe('Rp 0');
	});

	it('formats a small amount with no separator needed', () => {
		expect(formatRupiah(500)).toBe('Rp 500');
	});
});

function draft(overrides: Partial<PriceDraft> = {}): PriceDraft {
	return { ...newPriceDraft(), ...overrides };
}

describe('matchesFilter', () => {
	it('matches on routeCode, case-insensitively', () => {
		expect(matchesFilter(draft({ routeCode: 'JKT-BDG-01' }), 'jkt')).toBe(true);
	});

	it('matches on region', () => {
		expect(matchesFilter(draft({ region: 'Sumatra' }), 'sumat')).toBe(true);
	});

	it('matches on origin', () => {
		expect(matchesFilter(draft({ origin: 'Padang DC' }), 'padang')).toBe(true);
	});

	it('empty query matches everything', () => {
		expect(matchesFilter(draft(), '')).toBe(true);
	});

	it('no match returns false', () => {
		expect(matchesFilter(draft({ routeCode: 'JKT-BDG-01', region: 'Jawa', origin: 'Jakarta DC' }), 'zzz')).toBe(
			false
		);
	});
});

describe('priceDraftIsValid', () => {
	it('a fully-empty draft is invalid', () => {
		expect(priceDraftIsValid(newPriceDraft())).toBe(false);
	});

	it('valid once routeCode, origin, destinations, vehicleType are set and price is positive', () => {
		const valid = draft({
			routeCode: 'JKT-BDG-01',
			origin: 'Jakarta DC',
			destinations: ['Bandung DC'],
			vehicleType: 'TRONTON',
			price: 150000
		});
		expect(priceDraftIsValid(valid)).toBe(true);
	});

	it('invalid when price is zero or negative', () => {
		const zeroPrice = draft({
			routeCode: 'JKT-BDG-01',
			origin: 'Jakarta DC',
			destinations: ['Bandung DC'],
			vehicleType: 'TRONTON',
			price: 0
		});
		expect(priceDraftIsValid(zeroPrice)).toBe(false);
	});

	it('invalid when destinations is empty', () => {
		const noDest = draft({
			routeCode: 'JKT-BDG-01',
			origin: 'Jakarta DC',
			destinations: [],
			vehicleType: 'TRONTON',
			price: 150000
		});
		expect(priceDraftIsValid(noDest)).toBe(false);
	});

	it('region is never required (may stay empty)', () => {
		const noRegion = draft({
			routeCode: 'JKT-BDG-01',
			region: '',
			origin: 'Jakarta DC',
			destinations: ['Bandung DC'],
			vehicleType: 'TRONTON',
			price: 150000
		});
		expect(priceDraftIsValid(noRegion)).toBe(true);
	});
});
