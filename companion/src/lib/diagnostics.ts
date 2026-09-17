// "Copy diagnostics", which is what the Companion has instead of telemetry.
//
// Spec #65 is explicit: no telemetry and no third-party error collector. That
// leaves the user with nothing to send when something breaks, so they get a
// block of text they can read first and paste wherever they choose. Nothing
// here is transmitted; the only way it leaves the device is the user's own
// clipboard.
//
// What it must therefore *not* contain: the owner's Matrix ID, a device token
// (which is `HttpOnly` and unreadable anyway), a domain the user typed, or
// anything from a conversation. Versions, capability states and the browser's
// own self-description — enough to tell a stale shell from a hardened browser
// from an unreachable Gateway, and nothing a user would regret pasting into an
// issue.
//
// The builder is pure and unit-tested; `copyDiagnostics` is the browser half.

import type { CapabilityReport } from '$lib/capabilities/report';
import type { GatewayHealth, Handshake } from '$lib/version/handshake';

export interface DiagnosticsInput {
	/** The Companion's own build id (SvelteKit's `version`). */
	appBuild: string;
	/** The Gateway version this build was generated against. */
	expectedGatewayVersion: string;
	/** What `/health` said, or `null` when it did not answer. */
	health: GatewayHealth | null;
	handshake: Handshake;
	capabilities: CapabilityReport | null;
	locale: string;
	userAgent: string;
	/** `display-mode: standalone` — the app was opened from the home screen. */
	installed: boolean;
	/** Passed in rather than read, so the text is reproducible in a test. */
	generatedAt: Date;
}

/**
 * The text the button copies. Plain lines of `key: value`, because it is read
 * by a human in a chat window before it is read by anything else.
 */
export function buildDiagnostics(input: DiagnosticsInput): string {
	const lines: string[] = [
		'Twalk Companion diagnostics',
		`generated: ${input.generatedAt.toISOString()}`,
		'',
		`app build: ${input.appBuild}`,
		`built for gateway: ${input.expectedGatewayVersion}`,
		`gateway version: ${input.health?.version ?? 'unknown'}`,
		`gateway revision: ${input.health?.revision ?? 'unknown'}`,
		`version handshake: ${describeHandshake(input.handshake)}`,
		''
	];

	if (input.capabilities === null) {
		lines.push('capabilities: not measured yet');
	} else {
		lines.push(`capabilities cause: ${input.capabilities.cause}`);
		for (const row of input.capabilities.rows) {
			lines.push(`  ${row.id}: ${row.state}${row.required ? ' (required)' : ''}`);
		}
	}

	lines.push(
		'',
		`locale: ${input.locale}`,
		`installed: ${input.installed ? 'yes' : 'no'}`,
		`user agent: ${input.userAgent}`,
		'',
		'No telemetry is collected. This text was produced on request and has',
		'been sent nowhere.'
	);

	return lines.join('\n');
}

function describeHandshake(handshake: Handshake): string {
	switch (handshake.kind) {
		case 'checking':
			return 'not finished';
		case 'match':
			return `match (${handshake.version})`;
		case 'mismatch':
			return `mismatch (built for ${handshake.expected}, gateway ${handshake.actual})`;
		case 'unreachable':
			return `unreachable (${handshake.reason})`;
	}
}

/**
 * Puts the text on the clipboard. `false` when the browser refused, which is
 * normal outside a secure context and under some permission policies — the
 * page then shows the text for the user to select by hand.
 */
export async function copyDiagnostics(text: string): Promise<boolean> {
	try {
		await navigator.clipboard.writeText(text);
		return true;
	} catch {
		return false;
	}
}
