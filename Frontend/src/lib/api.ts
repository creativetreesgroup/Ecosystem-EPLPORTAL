// Frontend/src/lib/api.ts
// Thin fetch wrapper for calling reactor-core. Paths are RELATIVE — in production this goes
// through Caddy (docker/Caddyfile's @backend matcher, Task 2), in local `pnpm dev` through
// Vite's proxy (vite.config.ts, Task 3). Never construct an absolute backend URL here; that
// would bypass both routing layers and reintroduce the CORS problem they exist to avoid.
export class ApiError extends Error {
	constructor(
		public status: number,
		message: string
	) {
		super(message);
	}
}

export async function apiPost<T>(path: string, body: unknown): Promise<T> {
	const res = await fetch(path, {
		method: 'POST',
		credentials: 'include',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(body)
	});
	if (!res.ok) {
		throw new ApiError(res.status, 'request failed');
	}
	return res.json();
}
