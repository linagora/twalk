# A portal room is observed by invitation, and the choice is per conversation

A bridge's portal rooms are observed by **inviting the Sensor into them**, one conversation at a time, and the invitation is issued by the **bridge's own bot** through the appservice token the deployment already configures (`GATEWAY_BRIDGE_<ID>_AS_TOKEN`). The Companion Gateway holds a **portal register**: a live read of each bridge's portal rooms and of the Sensor's membership in each, with no store behind it. Nothing is observed by default, and the number of conversations the Sensor is *outside* is a fact the deployment states — in `GET /api/portals` and in `twalk_companion_gateway_portal_rooms`. This is the local half of the seam [ADR 0020](0020-observing-a-foreign-homeserver-is-a-device-of-the-users-account.md) decided for foreign homeservers, and it is decided with it ([#105](https://github.com/linagora/twalk/issues/105)).

## Whether ADR 0020's argument reaches here

It does not, and the reason is worth stating rather than assuming from the code that exists.

ADR 0020 rejected inviting `@sensor:<deployment>` into rooms on a homeserver the deployment does not run, on two grounds. The first is that it **cannot work**: the Sensor must join a room hosted elsewhere, which needs working federation in both directions, and a deployment reached on loopback through a tunnel is federatable by nobody. The second is that it is **the most exposed shape**: every colleague in that room sees an unfamiliar account on an unknown server join their conversation, which is exactly the third-party problem [#122](https://github.com/linagora/twalk/issues/122) is about.

Neither reaches a portal room. A portal is on the deployment's own homeserver, built by the deployment's own bridge, so no federation is involved at all — the step that could not land against a real corporate account is not on this path. And a portal room's members are the user, the bridge bot and the network ghosts the bridge materialised: **no third party is present**. The people in the WhatsApp group are on WhatsApp; they have no Matrix client, see no membership list and are told nothing by the Sensor's arrival. There is nobody in the room for an invitation to expose the deployment to.

So ADR 0020's conclusion — *a device of the user's own account* — solves a problem this side does not have, at a cost this side would still pay: a second sync loop, a second store, a long-lived token at rest, and cross-signing against a homeserver we do control but would gain nothing from. ADR 0020 already anticipated this, saying the Sensor's existing single identity "continues to serve the bridges' portal rooms". This ADR is that sentence made explicit and given a mechanism, so the project has **one observation story with two cases** — local rooms by invitation, foreign accounts by a device — rather than two that drifted apart.

What does **not** transfer from "no third party is present" is the conclusion people reach from it next. Those ghosts stand for real people. The eighteen portal rooms measured on the reference deployment held roughly 1,300 memberships, and observing a conversation publishes what those people wrote. Nobody in the room can see the Sensor; that is a reason the *invitation* is unobjectionable, and no reason at all that the *observation* is. Which is why the decision below is per conversation and defaults to none.

## Why the mechanism had to be continuous

The ticket's first framing — and the obvious fix — was to invite the Sensor into the rooms that exist when a network is connected. That is wrong, and the timestamps are what show it: the eighteen rooms were created progressively between 04:54 and 13:24 as conversations became active. mautrix builds portals **lazily**, as messages arrive. A one-off repair at connection time would have been correct for an hour and wrong by the evening, and the deafness grows on its own for as long as the deployment runs.

So the register is re-read rather than computed once. There is no push to subscribe to: a bridge announces a new portal by creating a room and inviting the user, and an appservice transaction goes to the appservice, which *is* the bridge. The Companion's read is live on every call, and a background read on an interval keeps the gauges honest without a browser. A portal created at 13:24 is offered immediately to anyone looking and is in `/metrics` by 13:29.

## Why the bridge bot, and not one of the alternatives

**The user's own Matrix session** is how `POST /api/bootstrap/rooms` invites the Sensor into rooms the user picked on screen 3d, and it is right there: the token is a parameter of one call and is forgotten when the call returns. It cannot be right here, because here the mechanism has to keep working when nobody is at a browser. Holding a Matrix access token of the user's would breach [ADR 0011](0011-gateway-authenticates-with-matrix-openid.md), which says the Gateway never holds one, and would create exactly the credential at rest that ADR 0020 records as a cost it accepts reluctantly.

**Telling the Sensor which bridge bots' portals to join** was the ticket's second option, and it does not work: the Sensor cannot see a room it is not in. It has no way to enumerate portals, and giving it the appservice token to do so would move the same credential to a second process while making the Sensor its own policy engine.

**A bridge setting** would be best of all, and there is none. mautrix invites the user and offers no way to name a third account.

The bridge bot is what remains, and it turns out to be the right answer rather than the surviving one. It is in every portal of its own bridge and in nobody else's rooms; it created those rooms and therefore has the power level to invite in them; and the credential already exists in the Gateway's configuration, so nothing new is stored and no secret is minted. Stopping observation is the same credential removing the Sensor from the room — the exact inverse, rather than a second control plane for a membership Matrix already models.

The change of posture is real and is the thing to argue with: ticket #56 put each bridge's `as_token` in the Gateway to **verify** that bridge's status pushes, and this ADR is the first thing that **acts** with it. An appservice token can act as any user in its namespace, so the Gateway now holds, for each bridge, a credential far broader than the use it is put to. Two narrowings are built in rather than promised: the register only ever speaks the client-server API as the token's own default identity — never `?user_id=`, never a ghost — and an observation request is resolved against the live register first, so a room id in a request can never send that credential somewhere its bot is not already. `docs/architecture/security-model.md` carries it as a residual risk.

## Why the register holds nothing

Whether a conversation is observed is the Sensor's own `m.room.member` event, asked of the homeserver on every read. There is no row, no cache and no second record. A reconnection that rebuilds every portal is simply the next answer; a replay and the list cannot disagree, because there is only one of them. This is the same posture as the suggestion listing ([ADR 0022](0022-the-approval-api-lives-on-the-companion-gateway.md)) and for the same reason, and it is what `CONTRIBUTING.md` asks of a test — prove a property against the system's own state rather than the interface — made true of the code as well.

The cost is that a read is one homeserver call per portal room, so the register is not free and is not a thing to poll from a screen. That is accepted: a personal deployment has tens of conversations, not thousands.

## What this ADR does not decide

**Which conversations to observe.** The mechanism is the Gateway's; the policy is the user's, expressed per conversation ([#143](https://github.com/linagora/twalk/issues/143)), and the default is none. The unit matters: for `maria` a contact and a conversation are the same thing and consent-per-contact works, and for a 246-member association nobody adjudicates 246 people one by one. That third unit of decision is #143's to introduce.

**Whether the people in an observed conversation should be told.** That is [#122](https://github.com/linagora/twalk/issues/122), and it is open. Nothing here bypasses consent: the Sensor joining a room grants nothing, and every sender still starts at `pending` ([ADR 0012](0012-revoked-consent-reduces-publication.md)).

**Three states, not two.** A portal is `observing`, `invited` or `absent`. `invited` is not a transient on the way to `observing`; it is what a deployment looks like when `SENSOR_ALLOWED_INVITERS` does not name the bridge bot. The invitation lands, the Sensor ignores it, and without that state the symptom would once again be a silence nothing accounts for. A portal stuck at `invited` names its own cause, and the Sensor counts its refusals (`twalk_sensor_invites_total{outcome="ignored"}`).
