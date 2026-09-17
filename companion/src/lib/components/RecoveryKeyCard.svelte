<!--
	Wireframe screen 2, second step: the recovery key, shown once.

	Everything the wireframe asks for is here — the key "in a large, readable
	font (mono, tabular numerals)" in groups of four, a copy button, a "Save as
	PDF" that writes a printable one-page document, and a checkbox that gates
	"Continue".

	Two things it does that the wireframe only implies, and ticket #67 makes
	explicit:

	- The copy says **what is lost** without the key, not merely that it cannot
	  be recovered: the conversation history stays encrypted for ever, and the
	  other devices can no longer be verified. It also states that there is no
	  reset flow in this version, because there is not one.
	- The key never leaves the page. It arrives as a string from
	  `$lib/crypto/bootstrap.ts` (whose callback is the only place the private
	  key exists outside the crypto stack), goes to the clipboard and into a
	  PDF built in this browser, and travels in no request.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { locale, t } from '$lib/i18n';
	import { copyRecoveryKey, downloadBytes, recoveryKeyFilename } from '$lib/recovery/download';
	import { keyRows, recoveryKeyPdf } from '$lib/recovery/pdf';

	interface Props {
		/** The key, already in 12 groups of four. */
		recoveryKey: string;
		userId: string;
		domain: string;
		/** Whether the key backup was started. Reported, never hidden. */
		keyBackup: boolean;
		/** Called when the user has ticked the box and pressed Continue. */
		onContinue: () => void;
	}

	let { recoveryKey, userId, domain, keyBackup, onContinue }: Props = $props();

	let saved = $state(false);
	let copied = $state<'idle' | 'done' | 'failed'>('idle');

	const rows = $derived(keyRows(recoveryKey));

	async function copy() {
		copied = (await copyRecoveryKey(recoveryKey)) ? 'done' : 'failed';
	}

	function savePdf() {
		const bytes = recoveryKeyPdf({
			groupedKey: recoveryKey,
			strings: {
				title: $t('pdf.title'),
				intro: $t('pdf.intro'),
				keyLabel: $t('pdf.keyLabel'),
				warning: $t('pdf.warning'),
				advice: [$t('pdf.advice1'), $t('pdf.advice2'), $t('pdf.advice3')],
				account: $t('pdf.account', { id: userId, domain }),
				generated: $t('pdf.generated', {
					date: new Date().toLocaleDateString($locale)
				})
			}
		});
		downloadBytes(bytes, recoveryKeyFilename(userId), 'application/pdf');
	}
</script>

<section class="stack" data-testid="screen-recovery-key">
	<header class="stack">
		<h1>{$t('key.title')}</h1>
		<p class="subtitle">{$t('key.intro')}</p>
	</header>

	<div class="card">
		<p class="card__title">
			<Icon name="recovery-key" size="dense" />
			{$t('key.label')}
		</p>
		<!-- `user-select: all` so a tap selects the whole key, which is what a
		     phone user without a working clipboard button has left. -->
		<p class="key mono" data-testid="recovery-key">
			{#each rows as row (row)}
				<span class="key__row" data-testid="recovery-key-row">{row}</span>
			{/each}
		</p>
		<p class="small muted">{$t('key.account', { id: userId })}</p>

		<div class="actions">
			<button class="button button--secondary" onclick={copy} data-testid="copy-recovery-key">
				<Icon name={copied === 'done' ? 'copied' : 'copy'} size="dense" />
				{copied === 'done' ? $t('key.copied') : $t('key.copy')}
			</button>
			<button class="button button--secondary" onclick={savePdf} data-testid="save-recovery-pdf">
				<Icon name="download" size="dense" />
				{$t('key.pdf')}
			</button>
		</div>
		{#if copied === 'failed'}
			<p class="error-text" role="alert">{$t('key.copyFailed')}</p>
		{/if}
	</div>

	<div class="card card--warning" data-testid="recovery-key-warning">
		<p class="card__title">
			<Icon name="warning" size="dense" />
			{$t('key.warning.title')}
		</p>
		<p>{$t('key.warning.body')}</p>
		<p><strong>{$t('key.warning.noReset')}</strong></p>
	</div>

	<p class="small muted" data-testid="key-backup-state">
		<Icon name={keyBackup ? 'ok' : 'attention'} size="dense" />
		{keyBackup ? $t('key.backup.on') : $t('key.backup.off')}
	</p>

	<label class="confirm">
		<input type="checkbox" bind:checked={saved} data-testid="saved-checkbox" />
		<span>{$t('key.checkbox')}</span>
	</label>

	<button
		class="button button--primary"
		disabled={!saved}
		onclick={onContinue}
		data-testid="recovery-key-continue"
	>
		{$t('key.continue')}
		<Icon name="continue" size="dense" />
	</button>
</section>

<style>
	.key {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		font-size: var(--text-lg);
		letter-spacing: 0.08em;
		user-select: all;
		overflow-wrap: anywhere;
	}

	.key__row {
		white-space: nowrap;
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.confirm {
		display: flex;
		align-items: flex-start;
		gap: var(--space-2);
		/* A 48 px target on a phone, which is the wireframes' floor. */
		min-height: 48px;
		padding: var(--space-2) 0;
	}

	.confirm input {
		width: 20px;
		height: 20px;
		margin-top: 2px;
		flex: none;
	}
</style>
