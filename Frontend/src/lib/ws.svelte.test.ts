// Frontend/src/lib/ws.svelte.test.ts
// Full WebSocket lifecycle needs a browser/e2e context (Task 9 covers that) — this test
// isolates just the backoff CALCULATION (deterministic, pure), the one piece of ws.svelte.ts
// worth a fast unit test on its own. Imports the real function (not a duplicate) so a change
// to the actual reconnect math trips this test.
import { describe, it, expect } from 'vitest';
import { backoffDelay } from './ws.svelte';

describe('reconnect backoff', () => {
	it('doubles each attempt starting from the base delay', () => {
		expect(backoffDelay(0)).toBe(1000);
		expect(backoffDelay(1)).toBe(2000);
		expect(backoffDelay(2)).toBe(4000);
		expect(backoffDelay(3)).toBe(8000);
	});

	it('caps at the max delay instead of growing unbounded', () => {
		expect(backoffDelay(10)).toBe(15000);
		expect(backoffDelay(100)).toBe(15000);
	});
});
