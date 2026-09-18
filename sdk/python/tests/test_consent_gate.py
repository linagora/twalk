"""The consent gate — the SDK's reason to exist.

The gate is also asserted at the seam (`hermes/tests/assistant.rs` drives
the real persona process and asserts a non-granted event produces no event
and no LLM call). These unit tests pin the decision itself, case by case,
including the ones a running system would rarely produce: the spec asks for
a gate that is "impossible for an author to forget", which means its rule
has to be stated somewhere it can be read in one screen.
"""

from __future__ import annotations

import unittest

from fixtures import fixture, variant_fixture
from twalk_sdk import consent_of, is_granted


class ConsentGateTest(unittest.TestCase):
    def test_a_granted_message_passes(self) -> None:
        event = fixture("inbound.message.received")
        self.assertEqual(event["consent"], "granted", "the fixture is the granted shape")
        self.assertTrue(is_granted(event))

    def test_a_pending_message_is_refused(self) -> None:
        event = fixture("inbound.message.received")
        event["consent"] = "pending"
        self.assertFalse(is_granted(event))

    def test_a_revoked_message_is_refused_without_reading_its_content(self) -> None:
        # The contract's own reduced shape: a revoked sender's message has
        # no body, no excerpt and no attachment reference (ADR 0012). The
        # gate must refuse it on the envelope alone, which is exactly what
        # makes it safe against a shape it never sees the inside of.
        event = variant_fixture("inbound.message.received", "revoked-sender")
        self.assertEqual(event["consent"], "revoked")
        self.assertNotIn("body", event["data"], "the reduced shape carries no body")
        self.assertFalse(is_granted(event))

    def test_the_decision_does_not_depend_on_a_body_being_present(self) -> None:
        # An envelope with no data at all still decides: the gate reads the
        # consent extension and nothing else, so it cannot come to depend
        # on a field a reduced event does not have.
        self.assertTrue(is_granted({"consent": "granted"}))
        self.assertFalse(is_granted({"consent": "revoked"}))

    def test_an_unreadable_consent_state_is_refused(self) -> None:
        # Not a denylist of pending and revoked: anything that is not
        # exactly "granted" is refused, because the safe default when
        # consent cannot be read is to not process the message.
        for event in (
            {},
            {"consent": None},
            {"consent": "Granted"},
            {"consent": " granted"},
            {"consent": "granted_for_now"},
            {"consent": True},
            {"consent": ["granted"]},
            {"data": {"consent": "granted"}},
        ):
            with self.subTest(event=event):
                self.assertFalse(is_granted(event))

    def test_consent_of_reports_the_state_it_read(self) -> None:
        self.assertEqual(consent_of({"consent": "pending"}), "pending")
        self.assertIsNone(consent_of({}))
        self.assertIsNone(consent_of({"consent": 3}))


if __name__ == "__main__":
    unittest.main()
