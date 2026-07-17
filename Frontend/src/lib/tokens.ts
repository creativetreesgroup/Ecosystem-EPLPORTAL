// Frontend/src/lib/tokens.ts
// Hand-kept mirror of Frontend/src/app.css's @theme block — for canvas/JS contexts (Latency
// Tape, 7b+) that can't reference CSS custom properties directly. Values MUST stay byte-identical
// to app.css; that file's own top comment points back here. No automated sync exists (deliberately
// simple for two small token sets — revisit if this drifts in practice across future sub-phases).
export const TOKENS = {
	dark: {
		bgBase: '#15181c',
		bgSurface: '#1c2025',
		border: '#262b31',
		textPrimary: '#f1f3f5',
		textMuted: '#8b95a1',
		accent: '#eab308',
		live: '#2dd4bf',
		danger: '#f87171'
	},
	light: {
		bgBase: '#f7f6f3',
		bgSurface: '#ffffff',
		border: '#e4e1da',
		textPrimary: '#1c1e21',
		textMuted: '#5c5952',
		accent: '#b45309',
		live: '#0d9488',
		danger: '#dc2626'
	}
} as const;

export type ThemeName = keyof typeof TOKENS;
