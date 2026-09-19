// The conversation chooser, end to end (ticket #143).
//
// # What this proves that nothing else can
//
// `companion-gateway/tests/portals_deployment.rs` already proves the
// *mechanism* on the reference deployment: a portal room built after everything
// was running is offered, choosing it puts the real Sensor inside it, and a
// message in it reaches the bus. This file does not repeat that with a second
// stack. What it adds is the half that lives in a browser, and which the ticket
// is actually about:
//
//   1. an account's conversations arrive **grouped** — one person, groups,
//      communities — from the network's own identifier and not from a guess;
//   2. a search filters across all of them;
//   3. the bulk control is scoped to what the filter shows, counted and named,
//      and "select all" is nowhere on the screen;
//   4. ticking a conversation that covers a crowd states how many people that
//      is **before** the tick takes effect, and will not be applied until the
//      number has been acknowledged;
//   5. and a conversation chosen here is one the real Sensor ends up inside and
//      publishing from — asserted at the homeserver and at the bus.
//
// Every count on screen is the homeserver's own: the crowded group really holds
// twenty-two accounts. A fixture that faked a member count would be asserting
// the screen against itself, and the criterion this screen exists for is
// precisely that the number is true.

import { expect, test } from '@playwright/test';

import { signIn } from '../networks/harness';
import { INBOUND_SUBJECT, watchBus } from '../dashboard/bus';
// One Sensor helper for the whole suite: #170's, because its durable consent
// consumer has a constant name and two Sensors on one bus split the consent
// stream between them. `playwright.config.ts` orders this project after
// `consent` so the two never overlap.
import { startSensor, type RunningSensor } from '../consent/sensor';
import {
	buildPortal,
	ghostSays,
	groupId,
	NO_STACK,
	personId,
	portalStack,
	sensorMembership,
	type BuiltPortal,
	type PortalStack
} from './harness';

const stack = portalStack();

test.describe('the conversation chooser', () => {
	test.skip(stack === null, NO_STACK);
	test.describe.configure({ mode: 'serial' });

	/**
	 * One account's conversations, in the shape the reference deployment's own
	 * WhatsApp account had (#105's measurements): a two-person conversation with
	 * somebody, a community that arrives as two groups with the same name, and
	 * an ordinary group. Built once for the whole file — the register is a live
	 * read, so these are simply what this bridge holds from here on.
	 */
	let account: {
		maria: BuiltPortal;
		community: BuiltPortal;
		announcements: BuiltPortal;
		team: BuiltPortal;
	};

	test.beforeAll(async () => {
		// Belt as well as braces: `npm test` has to stay a Node-only suite, and
		// a hook that built rooms against a homeserver that is not there would
		// fail the run rather than skip it.
		if (stack === null) {
			return;
		}
		// Four rooms and twenty-five real memberships against a homeserver that
		// may have started thirty seconds ago. The default hook budget is the
		// same thirty seconds, which is a race rather than a limit.
		test.setTimeout(180_000);
		const real = stack as PortalStack;
		const { ghost, crowd } = real.portals;
		account = {
			maria: await buildPortal(real, 'maria (WA)', personId('33612345678'), [ghost]),
			// 06:46, two rooms, one name: a community and its announcement group.
			// The member counts are what tell them apart, and they are real.
			community: await buildPortal(real, 'Communauté CKCP', groupId(1), crowd),
			announcements: await buildPortal(real, 'Communauté CKCP', groupId(2), [ghost]),
			team: await buildPortal(real, 'Linagora : Team Clean', groupId(3), [ghost])
		};
	});

	test('groups an account by what the network says each conversation is', async ({
		page,
		context,
		request
	}) => {
		await signIn(context, request, 'portals-grouping');
		await page.goto('/networks/conversations?network=whatsapp');
		await expect(page.getByTestId('screen-conversations')).toHaveAttribute('data-loaded', 'yes');

		// The threshold the crowds are drawn from is the Gateway's, served with
		// the register, and not a number of this screen's (#252): the stack
		// starts its Gateway with 21, which no default would produce.
		await expect(page.getByTestId('screen-conversations')).toHaveAttribute(
			'data-crowd-threshold',
			'21'
		);

		// One person, because `…@s.whatsapp.net` says one person. Not because it
		// has two members — a group somebody left has one too.
		const oneToOne = page.getByTestId('section-one-to-one');
		await expect(oneToOne).toContainText('maria (WA)');

		// The community: two rooms with the same name, presented as one thing,
		// with the people it covers across both.
		const community = page.getByTestId('section-community');
		await expect(community).toBeVisible();
		await expect(community).toContainText('Communauté CKCP');
		await expect(community).toContainText(
			`${(stack as PortalStack).portals.crowd.length + 1} people`
		);
		// And the screen says plainly that this grouping is by name, because the
		// network never says which groups form a community.
		await expect(page.getByTestId('community-caveat')).toBeVisible();

		// The ordinary group is not in the community section, and the community's
		// two rooms are not in the group section.
		const groups = page.getByTestId('section-group');
		await expect(groups).toContainText('Linagora : Team Clean');
		await expect(groups).not.toContainText('Communauté CKCP');

		// The two same-named rows are distinguishable, which is the whole reason
		// the network's own identifier is on the screen.
		await expect(page.getByTestId(`conversation-${account.community.roomId}`)).toBeVisible();
		await expect(page.getByTestId(`conversation-${account.announcements.roomId}`)).toBeVisible();
		await expect(community).toContainText(account.community.conversationId);
		await expect(community).toContainText(account.announcements.conversationId);

		// Nothing is watched until the user says so.
		await expect(page.getByTestId('conversations-summary')).toContainText('reading 0');
	});

	test('scopes the bulk control to the filter, counted and named', async ({
		page,
		context,
		request
	}) => {
		await signIn(context, request, 'portals-bulk');
		await page.goto('/networks/conversations?network=whatsapp');
		await expect(page.getByTestId('screen-conversations')).toHaveAttribute('data-loaded', 'yes');

		const control = page.getByTestId('conversations-toggle-shown');
		const everything = await control.innerText();

		await page.getByTestId('conversation-search').fill('ckcp');
		await expect(page.getByTestId('conversations-count')).toContainText('Showing 2 of');
		// Counted and named. Never "select all" — over a list that can contain a
		// 246-member association, that is the affordance this ticket exists to
		// avoid (#122).
		await expect(control).toHaveText(/Select the 2 shown/);
		expect(everything).not.toEqual(await control.innerText());
		await expect(page.locator('body')).not.toContainText('Select all');

		await control.click();
		// It touched the two the filter was showing and nothing else: the team
		// and maria are still unticked after the search is cleared.
		await page.getByTestId('conversation-search').fill('');
		await expect(page.getByTestId(`conversation-${account.community.roomId}`)).toBeChecked();
		await expect(page.getByTestId(`conversation-${account.announcements.roomId}`)).toBeChecked();
		await expect(page.getByTestId(`conversation-${account.team.roomId}`)).not.toBeChecked();
		await expect(page.getByTestId(`conversation-${account.maria.roomId}`)).not.toBeChecked();
	});

	test('states how many people a community covers before the tick takes effect', async ({
		page,
		context,
		request
	}) => {
		const real = stack as PortalStack;
		const people = real.portals.crowd.length + 1;
		await signIn(context, request, 'portals-consequence');
		await page.goto('/networks/conversations?network=whatsapp');
		await expect(page.getByTestId('screen-conversations')).toHaveAttribute('data-loaded', 'yes');

		// The count is on the row before anything is ticked, and it is the
		// homeserver's own: twenty-two accounts really joined that room.
		await expect(page.getByTestId(`members-${account.community.roomId}`)).toHaveText(
			`${real.portals.crowd.length} people`
		);

		const apply = page.getByTestId('conversations-apply');
		await expect(apply).toBeDisabled();

		// One gesture over the whole community — and its own label says how many
		// people that gesture covers.
		const family = page.getByTestId('section-community').getByRole('button', {
			name: /Watch all 2/
		});
		await expect(family).toContainText(`${people} people`);
		await family.click();

		// The consequence, stated while the decision is still pending.
		await expect(page.getByTestId('consequence-adding')).toContainText(`${people} people`);
		await expect(page.getByTestId('conversations-consequence')).toContainText('Communauté CKCP');

		// And it will not be sent until the number has been acknowledged. A
		// number is not a warning, and a warning nobody reads is not a decision.
		const acknowledgement = page.getByTestId('conversations-acknowledge');
		await expect(acknowledgement).toBeVisible();
		await expect(acknowledgement).toContainText(`${people} people`);
		await expect(apply).toBeDisabled();

		await page.getByTestId('conversations-acknowledged').check();
		await expect(apply).toBeEnabled();

		// Changing the decision asks again: the count they agreed to is not the
		// count they now have.
		await page.getByTestId(`conversation-${account.maria.roomId}`).check();
		await expect(apply).toBeDisabled();
	});

	test('a conversation chosen here is one the Sensor joins and publishes from', async ({
		page,
		context,
		request
	}) => {
		const real = stack as PortalStack;
		// Three processes have to agree before this test can finish: the
		// Gateway issues the invitation, the Sensor notices it on its next
		// sync and joins, and the message that follows has to be normalised
		// and published. Measured at two to four seconds end to end — plus a
		// `cargo build` of the Sensor on a cold target directory, which is
		// minutes. The default thirty is spent before any of that starts.
		test.setTimeout(600_000);

		// A conversation that becomes active after everything else was already
		// running and already asserted — which is the case a one-off repair at
		// connection time never covers (ADR 0024).
		const chess = await buildPortal(real, 'Échecs en Yvelines', groupId(9), [
			real.portals.ghost
		]);

		// The real Sensor, started here rather than with the stack: it accepts
		// an invitation from this bridge bot and from nobody else, which is
		// also what keeps it out of every room the other journeys built.
		let sensor: RunningSensor | null = null;
		// Subscribed before the decision, so the event cannot arrive unseen.
		const bus = await watchBus(real.natsPort, INBOUND_SUBJECT);
		try {
			sensor = await startSensor({
				synapseUrl: real.synapseUrl,
				serverName: real.serverName,
				natsPort: real.natsPort,
				allowedInviters: [real.portals.bot]
			});
			expect(sensor.userId).toBe(real.portals.sensorId);

			await signIn(context, request, 'portals-journey');
			await page.goto('/networks/conversations?network=whatsapp');
			await expect(page.getByTestId('screen-conversations')).toHaveAttribute(
				'data-loaded',
				'yes'
			);

			// Before: the Sensor is in no conversation, and the homeserver is
			// where that is true.
			expect(await sensorMembership(real, chess.roomId)).toBeNull();

			await page.getByTestId('conversation-search').fill('yvelines');
			await expect(page.getByTestId('conversations-count')).toContainText('Showing 1 of');
			await page.getByTestId(`conversation-${chess.roomId}`).check();
			// One member, so no crowd to acknowledge: the gate is about the
			// number and not about the act.
			await expect(page.getByTestId('conversations-acknowledge')).toHaveCount(0);

			await page.getByTestId('conversations-apply').click();
			await expect(page.getByTestId(`outcome-${chess.roomId}`)).toHaveAttribute(
				'data-status',
				'invited'
			);

			// The Sensor joins on its own, because it accepts this bridge bot as
			// an inviter. Asked of the homeserver, not of the Gateway.
			await expect
				.poll(async () => sensorMembership(real, chess.roomId), { timeout: 60_000 })
				.toBe('join');

			// And what is written there reaches the bus. The contact is at
			// `pending`, so the event carries no body at all (ADR 0012) — the
			// room is what identifies it, in `source`.
			await ghostSays(real, chess.roomId, real.portals.ghost, 'On décale à 20h ?');
			const published = await bus
				.waitFor((message) => message.event.source.endsWith(chess.roomId), 60_000)
				.catch((error: unknown) => {
					// The Sensor's own log, or nobody can tell "it never saw the
					// room" from "it saw it and refused to publish". The helper
					// collects it for exactly this.
					throw new Error(
						`${String(error)}\n\nthe Sensor said:\n${(sensor?.log ?? []).join('\n')}`
					);
				});
			expect(published.event.type).toBe('fr.linagora.twalk.inbound.message.received.v1');

			// The screen now says so, and so the deployment can.
			await page.reload();
			await expect(page.getByTestId('conversations-summary')).toContainText('reading 1');
			await expect(page.getByTestId(`conversation-${chess.roomId}`)).toBeChecked();
		} finally {
			bus.close();
			// Reaped whatever happened: a Sensor left running would follow this
			// stack's bus into the next project's assertions.
			await sensor?.stop();
		}
	});

	test('a bridge with no portal register is reported, never rendered as silence', async ({
		page,
		context,
		request
	}) => {
		// Two of the three bridges on this stack have no appservice token, so
		// none of their conversations can be in the list. A count that quietly
		// covered fewer bridges than the user has connected would be this
		// ticket's own defect in another form.
		await signIn(context, request, 'portals-unreadable');
		await page.goto('/networks/conversations');
		await expect(page.getByTestId('screen-conversations')).toHaveAttribute('data-loaded', 'yes');
		await expect(page.getByTestId('bridge-unreadable-mautrix-signal')).toBeVisible();
		await expect(page.getByTestId('bridge-unreadable-mautrix-gmessages')).toContainText(
			'GATEWAY_BRIDGE_MAUTRIX_GMESSAGES_AS_TOKEN'
		);
	});
});
