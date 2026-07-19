// Frontend/src/lib/api-command.ts
// Thin typed REST layer for /command's KPI widgets — no UI logic here, matching
// api-bookings.ts/api-tickets.ts's established convention.
import { ApiError } from './api';

type BookingSummaryWire = {
	incoming_today: number;
	accepted_auto_today: number;
	accepted_manual_today: number;
	taken_by_other_today: number;
	latency_p99_ms: number | null;
};

export type CommandSummary = {
	incomingToday: number;
	acceptedAutoToday: number;
	acceptedManualToday: number;
	takenByOtherToday: number;
	latencyP99Ms: number | null;
};

export async function fetchSummary(): Promise<CommandSummary> {
	const res = await fetch('/bookings/summary', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch summary');
	const w: BookingSummaryWire = await res.json();
	return {
		incomingToday: w.incoming_today,
		acceptedAutoToday: w.accepted_auto_today,
		acceptedManualToday: w.accepted_manual_today,
		takenByOtherToday: w.taken_by_other_today,
		latencyP99Ms: w.latency_p99_ms
	};
}

export async function fetchVehicleTypes(): Promise<string[]> {
	const res = await fetch('/bookings/vehicle-types', { credentials: 'include' });
	if (!res.ok) throw new ApiError(res.status, 'failed to fetch vehicle types');
	return res.json();
}
