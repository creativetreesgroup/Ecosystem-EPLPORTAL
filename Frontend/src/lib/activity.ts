// Pure logic for the /activity page — no fetch, no DOM. Wire-format mapping lives in
// api-activity.ts, matching the established $lib "logic vs. REST layer" split from prior phases.

const OUTCOME_LABELS: Record<string, string> = {
	accepted: 'Diterima',
	rejected: 'Ditolak',
	skipped: 'Dilewati',
	taken_by_agency: 'Diambil Agensi Lain',
	failed: 'Gagal',
	agency_dup_unverified: 'Duplikat Agensi (Belum Terverifikasi)'
};

/** Falls back to the raw value for anything outside the known 6 — defensive only, the DB CHECK
 * constraint (migration 0008) means this should never actually happen in practice. */
export function outcomeLabel(outcome: string): string {
	return OUTCOME_LABELS[outcome] ?? outcome;
}

const LOG_TYPE_LABELS: Record<string, string> = {
	success: 'Berhasil',
	error: 'Gagal'
};

export function logTypeLabel(logType: string): string {
	return LOG_TYPE_LABELS[logType] ?? logType;
}

const KIND_LABELS: Record<string, string> = {
	accept: 'Terima Otomatis',
	agency_loss: 'Kalah dari Agensi',
	otp: 'OTP'
};

export function kindLabel(kind: string | null): string {
	if (kind === null) return 'Lainnya';
	return KIND_LABELS[kind] ?? kind;
}

const TIMESTAMP_FORMATTER = new Intl.DateTimeFormat('id-ID', {
	dateStyle: 'medium',
	timeStyle: 'short'
});

export function formatTimestamp(date: Date): string {
	return TIMESTAMP_FORMATTER.format(date);
}

export function formatMicroseconds(us: number | null): string {
	return us === null ? '—' : `${us} µs`;
}

export function formatMilliseconds(ms: number | null): string {
	return ms === null ? '—' : `${ms} ms`;
}
