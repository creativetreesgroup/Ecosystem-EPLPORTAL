// Pure formatting for Deadline Bidding's countdown — both the large ("3h 34m" style) and small
// "STANDBY" badge in TicketsTable read the SAME deadline_at value through this one function
// (confirmed with the user during design: there is no second, distinct deadline field).
export function formatCountdown(
	targetIso: string,
	nowMs: number
): { label: string; expired: boolean } {
	const deltaMs = Date.parse(targetIso) - nowMs;
	if (deltaMs <= 0) return { label: '00:00', expired: true };
	const totalSeconds = Math.floor(deltaMs / 1000);
	const hours = Math.floor(totalSeconds / 3600);
	const minutes = Math.floor((totalSeconds % 3600) / 60);
	const seconds = totalSeconds % 60;
	if (hours > 0) return { label: `${hours}h ${minutes}m`, expired: false };
	const pad = (n: number) => String(n).padStart(2, '0');
	return { label: `${pad(minutes)}:${pad(seconds)}`, expired: false };
}
