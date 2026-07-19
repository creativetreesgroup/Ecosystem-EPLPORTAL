import { describe, it, expect, vi, afterEach } from 'vitest';
import { fetchBotSettings, saveBotSettings } from './api-bot-settings';

afterEach(() => {
	vi.unstubAllGlobals();
});

function botSettingsWire(overrides: Partial<Record<string, unknown>> = {}) {
	return {
		enabled: true,
		webhook_url: 'https://n8n.example.com/webhook',
		wa_number: '628111111111',
		wa_group: '',
		waha_url: 'http://127.0.0.1:19999',
		waha_session: 'default',
		waha_api_key_set: true,
		waha_api_key: '',
		...overrides
	};
}

describe('fetchBotSettings', () => {
	it('issues a GET to /bot/settings and maps every snake_case field to camelCase', async () => {
		let calledUrl: string | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string) => {
				calledUrl = url;
				return new Response(JSON.stringify(botSettingsWire()), { status: 200 });
			})
		);
		const settings = await fetchBotSettings();
		expect(calledUrl).toBe('/bot/settings');
		expect(settings).toEqual({
			enabled: true,
			webhookUrl: 'https://n8n.example.com/webhook',
			waNumber: '628111111111',
			waGroup: '',
			wahaUrl: 'http://127.0.0.1:19999',
			wahaSession: 'default',
			wahaApiKeySet: true
		});
	});

	it('throws ApiError with the real status code on a non-ok response (e.g. 403)', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 403 })));
		await expect(fetchBotSettings()).rejects.toMatchObject({ status: 403 });
	});
});

describe('saveBotSettings', () => {
	it('issues a PUT (not POST) with a snake_case body matching BotSettingsRequest exactly', async () => {
		let calledUrl: string | undefined;
		let calledInit: RequestInit | undefined;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (url: string, init?: RequestInit) => {
				calledUrl = url;
				calledInit = init;
				return new Response(JSON.stringify(botSettingsWire()), { status: 200 });
			})
		);
		await saveBotSettings({
			enabled: true,
			webhookUrl: 'https://n8n.example.com/webhook',
			waNumber: '628111111111',
			waGroup: '',
			wahaUrl: 'http://127.0.0.1:19999',
			wahaSession: 'default',
			wahaApiKeySet: true,
			wahaApiKey: ''
		});
		expect(calledUrl).toBe('/bot/settings');
		expect(calledInit?.method).toBe('PUT');
		expect(JSON.parse(calledInit?.body as string)).toEqual(botSettingsWire());
	});

	it('sends a blank waha_api_key as an actual empty string, not omitted, when keeping the existing key', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async (_url: string, init?: RequestInit) => {
				const body = JSON.parse(init?.body as string);
				expect(body).toHaveProperty('waha_api_key', '');
				return new Response(JSON.stringify(botSettingsWire()), { status: 200 });
			})
		);
		await saveBotSettings({
			enabled: true,
			webhookUrl: '',
			waNumber: '',
			waGroup: '',
			wahaUrl: '',
			wahaSession: '',
			wahaApiKeySet: true,
			wahaApiKey: ''
		});
	});

	it('throws ApiError on a non-ok response', async () => {
		vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 400 })));
		await expect(
			saveBotSettings({
				enabled: false,
				webhookUrl: '',
				waNumber: '',
				waGroup: '',
				wahaUrl: '',
				wahaSession: '',
				wahaApiKeySet: false,
				wahaApiKey: ''
			})
		).rejects.toThrow();
	});
});
