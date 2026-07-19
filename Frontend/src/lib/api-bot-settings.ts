import { ApiError } from './api';

export type BotSettings = {
	enabled: boolean;
	webhookUrl: string;
	waNumber: string;
	waGroup: string;
	wahaUrl: string;
	wahaSession: string;
	wahaApiKeySet: boolean;
};

export type BotSettingsInput = BotSettings & { wahaApiKey: string };

type BotSettingsWire = {
	enabled: boolean;
	webhook_url: string;
	wa_number: string;
	wa_group: string;
	waha_url: string;
	waha_session: string;
	waha_api_key_set: boolean;
};

function fromWire(wire: BotSettingsWire): BotSettings {
	return {
		enabled: wire.enabled,
		webhookUrl: wire.webhook_url,
		waNumber: wire.wa_number,
		waGroup: wire.wa_group,
		wahaUrl: wire.waha_url,
		wahaSession: wire.waha_session,
		wahaApiKeySet: wire.waha_api_key_set
	};
}

export async function fetchBotSettings(): Promise<BotSettings> {
	const res = await fetch('/bot/settings', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch bot settings');
	const wire: BotSettingsWire = await res.json();
	return fromWire(wire);
}

/** `apiPost` (Frontend/src/lib/api.ts) hardcodes `method: 'POST'` — the backend route is
 * `PUT /bot/settings` (Backend/crates/api-gateway/src/routes/bot.rs's `bot_router`), so this
 * cannot use `apiPost`; a POST here would 405. Raw `fetch` with `method: 'PUT'`, same header/
 * credentials/error shape as `apiPost` otherwise. */
export async function saveBotSettings(input: BotSettingsInput): Promise<BotSettings> {
	const res = await fetch('/bot/settings', {
		method: 'PUT',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({
			enabled: input.enabled,
			webhook_url: input.webhookUrl,
			wa_number: input.waNumber,
			wa_group: input.waGroup,
			waha_url: input.wahaUrl,
			waha_session: input.wahaSession,
			waha_api_key_set: input.wahaApiKeySet,
			waha_api_key: input.wahaApiKey
		})
	});
	if (!res.ok) throw new ApiError(res.status, 'failed to save bot settings');
	const wire: BotSettingsWire = await res.json();
	return fromWire(wire);
}
