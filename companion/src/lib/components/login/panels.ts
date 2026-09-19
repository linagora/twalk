// Every kind of step, and the panel that draws it.
//
// This table is the whole of ADR 0030's exhaustiveness decision. Its type is a
// mapped type over `LoginView`'s own discriminant, so a kind added to the view
// and drawn by nobody is a **missing property here** — a compile error in the
// one file that could have forgotten it, which is the guarantee
// `login-view.ts`'s header used to claim and a Svelte `{#if}` chain cannot
// give.
//
// Two kinds may share a panel, by accepting the union of their views:
//
//   - `idle` and `starting` are one thing to the user: nothing to act on yet.
//   - `input` and `cookies` are one thing to the renderer: a question with a
//     field list. That the list happens to be all cookies is a fact about the
//     types in it, which is why the renderer is field-driven and not a special
//     case per network.

import type { PanelTable } from '$lib/networks/login-panels';

import CancelledPanel from './CancelledPanel.svelte';
import CompletePanel from './CompletePanel.svelte';
import EmojiPanel from './EmojiPanel.svelte';
import FailedPanel from './FailedPanel.svelte';
import FieldsPanel from './FieldsPanel.svelte';
import QrPanel from './QrPanel.svelte';
import RefusedPanel from './RefusedPanel.svelte';
import ResidualPanel from './ResidualPanel.svelte';
import VerifyingPanel from './VerifyingPanel.svelte';
import WaitingPanel from './WaitingPanel.svelte';

export const PANELS: PanelTable = {
	idle: WaitingPanel,
	starting: WaitingPanel,
	qr: QrPanel,
	emoji: EmojiPanel,
	verifying: VerifyingPanel,
	input: FieldsPanel,
	cookies: FieldsPanel,
	complete: CompletePanel,
	failed: FailedPanel,
	refused: RefusedPanel,
	unknown_step: ResidualPanel,
	cancelled: CancelledPanel
};
