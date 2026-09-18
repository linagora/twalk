"""The trigger-type gate — the second thing an author cannot forget.

The consent gate answers "may this be processed?". This one answers the
question before it: "may a persona be woken by this at all?" — and it exists
because of one event the consent gate structurally cannot stop. A message the
**user** sent (`outbound.message.sent`, ADR 0018) is on the bus so a persona
can know a conversation has already been answered, and it carries no consent
extension at all, because the extension is a contact's decision and there is
no contact in it. A gate that reads consent has nothing to read.

Asserted at the seam too (`hermes/tests/assistant.rs`, where the real persona
container is given the user's own message and produces no event and no LLM
call). These unit tests pin the decision itself, case by case.
"""

from __future__ import annotations

import unittest

from fixtures import fixture, variant_fixture
from twalk_sdk import (
    MESSAGE_RECEIVED_TYPE,
    OUTBOUND_MESSAGE_SENT_TYPE,
    PERSONA_TRIGGER_TYPES,
    is_granted,
    triggers_a_persona,
    type_of,
)


class TriggerGateTest(unittest.TestCase):
    def test_an_inbound_message_triggers_a_persona(self) -> None:
        event = fixture("inbound.message.received")
        self.assertEqual(event["type"], MESSAGE_RECEIVED_TYPE)
        self.assertTrue(triggers_a_persona(event))

    def test_the_users_own_message_does_not(self) -> None:
        event = fixture("outbound.message.sent")
        self.assertEqual(event["type"], OUTBOUND_MESSAGE_SENT_TYPE)
        self.assertFalse(triggers_a_persona(event))

    def test_the_consent_gate_alone_could_not_have_stopped_it(self) -> None:
        # The reason this gate exists, stated as an assertion. The user's own
        # message has no consent extension, so `is_granted` refuses it — but
        # by the accident of a missing attribute rather than by a decision
        # about whose message it is, and it would report "consent is not
        # granted" about an event that has no consent to be granted.
        event = fixture("outbound.message.sent")
        self.assertNotIn(
            "consent", event, "ADR 0018: this type carries no consent extension at all"
        )
        self.assertFalse(is_granted(event))
        # And under the shape ADR 0018 rejected — the user's own traffic kept
        # inside inbound.message.received — the consent gate would have said
        # yes, because the user's own consent is the most granting there is.
        as_inbound = dict(event, type=MESSAGE_RECEIVED_TYPE, consent="granted")
        self.assertTrue(is_granted(as_inbound))
        self.assertTrue(
            triggers_a_persona(as_inbound),
            "the type is the only attribute that tells the two apart, which is "
            "why ADR 0018 made it a type of its own",
        )

    def test_a_granted_reaction_does_not_trigger_a_persona(self) -> None:
        # An allowlist, not a denylist of the user's own messages: no other
        # contract type wakes a persona either, whatever its consent says.
        for type_name in (
            "inbound.reaction.added",
            "inbound.presence.updated",
            "persona.suggest.produced",
            "persona.thinking.emitted",
            "persona.reply.approved",
            "consent.state.changed",
            "bridge.status.changed",
        ):
            with self.subTest(type_name=type_name):
                self.assertFalse(triggers_a_persona(fixture(type_name)))

    def test_a_type_the_contract_adds_later_does_not_trigger_a_persona(self) -> None:
        # The forward-compatible default is "no". A tenth type must not start
        # waking personas because nobody thought to exclude it.
        self.assertFalse(
            triggers_a_persona({"type": "fr.linagora.twalk.inbound.call.missed.v1"})
        )

    def test_an_unreadable_type_does_not_trigger_a_persona(self) -> None:
        self.assertFalse(triggers_a_persona({}))
        self.assertFalse(triggers_a_persona({"type": None}))
        self.assertFalse(triggers_a_persona({"type": 42}))
        self.assertFalse(triggers_a_persona({"type": ["a", "list"]}))
        self.assertIsNone(type_of({"type": 42}))

    def test_a_reduced_event_still_decides_on_the_envelope_alone(self) -> None:
        # Like the consent gate: the decision never reaches inside `data`, so
        # a shape with no body cannot break it.
        event = variant_fixture("inbound.message.received", "revoked-sender")
        self.assertNotIn("body", event["data"])
        self.assertTrue(triggers_a_persona(event))
        self.assertFalse(is_granted(event), "and the consent gate then refuses it")

    def test_exactly_one_type_triggers_a_persona_today(self) -> None:
        self.assertEqual(PERSONA_TRIGGER_TYPES, frozenset({MESSAGE_RECEIVED_TYPE}))


if __name__ == "__main__":
    unittest.main()
