// The two browser-side halves of "save your recovery key": the clipboard and
// the download. Both deliberately tiny, both here rather than in the component,
// so the screen holds no browser API and the failure paths are named.
//
// The download is a `blob:` URL the page revokes itself. It never touches the
// network: the bytes are built in the page by `./pdf.ts` and handed to the
// browser's own save dialogue. That matters for more than tidiness — a
// recovery key that reached a server to be turned into a PDF would break the
// promise screen 2 makes, whichever server it was.

/** Writes the key to the clipboard. `false` when the browser refused. */
export async function copyRecoveryKey(key: string): Promise<boolean> {
	try {
		await navigator.clipboard.writeText(key);
		return true;
	} catch {
		// Safari refuses outside a user gesture, and a page without clipboard
		// permission refuses always. The key stays on screen and selectable.
		return false;
	}
}

/**
 * Offers the bytes as a file. `true` once the browser has been asked; a
 * browser that blocks programmatic downloads is the one case this cannot tell
 * apart from success, which is why the screen keys nothing off it.
 */
export function downloadBytes(bytes: Uint8Array, filename: string, type: string): boolean {
	try {
		const blob = new Blob([bytes as BlobPart], { type });
		const url = URL.createObjectURL(blob);
		const anchor = document.createElement('a');
		anchor.href = url;
		anchor.download = filename;
		anchor.rel = 'noopener';
		document.body.append(anchor);
		anchor.click();
		anchor.remove();
		// Revoked on the next turn of the event loop: revoking it immediately
		// races the browser's own fetch of the blob in some versions.
		setTimeout(() => URL.revokeObjectURL(url), 10_000);
		return true;
	} catch {
		return false;
	}
}

/**
 * A filename that names the account without being a full Matrix ID: a file in
 * a downloads folder is read by whoever can see the folder.
 */
export function recoveryKeyFilename(userId: string): string {
	const localpart = /^@([^:]+):/u.exec(userId)?.[1] ?? 'account';
	return `twalk-recovery-key-${localpart.replace(/[^a-z0-9_-]/giu, '')}.pdf`;
}
