// The seam between the component that owns a login's **flow** and the ones that
// draw each **step kind** (ADR 0030).
//
// # Why a table and not a chain of `{#if}`
//
// `login-view.ts` used to promise that a state added to the Gateway would make
// the screens fail to compile rather than fall through to a blank page. Svelte
// gives a `{#if}` chain no exhaustiveness check of any kind, so the promise was
// aspirational, and the thing it promised would not happen happened: the QR
// screen had no `{:else}`, and an `input`, `cookies` or `emoji` step drew an
// empty seventeen-rem box that the session polled once a second for ever.
//
// [`PanelTable`] is a mapped type over the view's own discriminant. A kind
// added to [`LoginView`] and drawn by nobody is a **missing property**, which is
// a compile error in the one file that could have forgotten it. That is the
// guarantee, stated where it is real.
//
// # And the residual, which no type can remove
//
// The wire is not the type system: a Gateway newer than this build can answer a
// step type this Companion has never heard of. `viewOf` reads that as
// `unknown_step` and the table draws it — the one panel whose job is to say
// plainly that the deployment is half-upgraded, rather than to draw nothing.

import type { Component } from 'svelte';

import type { LoginScreenCopy } from './copy';
import type { LoginView } from './login-view';

/** What a panel may do to the login it is drawing. The flow owns all of it. */
export interface FlowActions {
	/** Start over: the way out of every dead end on the screen. */
	readonly restart: () => Promise<void>;
	/** An **explicit** abandon, which cancels the bridge's process. */
	readonly cancel: () => Promise<void>;
	/** Answer the step. The data is the bridge's shape, built by `step-fields.ts`. */
	readonly submit: (stepId: string, data: Record<string, unknown>) => Promise<void>;
}

/** The view of one kind, or of the few kinds one panel serves. */
export type ViewOf<K extends LoginView['kind']> = Extract<LoginView, { kind: K }>;

/**
 * What a panel is given.
 *
 * Parameterised by the **view** and not by its kind, and a type alias rather
 * than an interface, both deliberately: TypeScript compares two references to a
 * generic interface by its measured variance, and a `kind` sitting inside a
 * conditional type makes that invariant — so a panel accepting two kinds could
 * not be registered for either of them. With the resolved view as the parameter,
 * the comparison is structural, which is what makes "one panel, several kinds"
 * expressible at all.
 */
export type PanelProps<V extends LoginView = LoginView> = {
	readonly view: V;
	readonly copy: LoginScreenCopy;
	readonly flow: FlowActions;
	/**
	 * The clock, ticked by the owner.
	 *
	 * Passed in rather than read: a countdown is the only thing on these screens
	 * that depends on the instant, and one ticker for the whole flow beats one
	 * per panel that mounts.
	 */
	readonly now: number;
};

/**
 * A panel that draws one kind of step.
 *
 * A panel may serve several kinds by accepting their union — the waiting panel
 * draws `idle` and `starting`, and one fields panel draws `input` and `cookies`,
 * because a jar of cookies is a field list like any other.
 */
export type StepPanel<V extends LoginView = LoginView> = Component<PanelProps<V>>;

/** Every kind of step, and the panel that draws it. */
export type PanelTable = { [K in LoginView['kind']]: StepPanel<ViewOf<K>> };
