"""The envelopes and their deterministic ids, against the contract's own
fixtures."""

from __future__ import annotations

import hashlib
import re
import unittest

from fixtures import fixture
from twalk_sdk import (
    EnvelopeError,
    InboundMessage,
    Suggestion,
    suggest_event,
    suggest_id,
    thinking_event,
    thinking_id,
)
from twalk_sdk import DISCLOSURE_MAX_CHARS, SENTENCES
from twalk_sdk.envelope import MAX_BODY_CHARS, MAX_RATIONALE_CHARS, nats_headers

SOURCE = "hermes://twalk.example.com/personas/assistant"
RFC3339 = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")


def trigger() -> InboundMessage:
    return InboundMessage(fixture("inbound.message.received"))


class DeterministicIdTest(unittest.TestCase):
    def test_the_thinking_id_is_the_contract_fixture_id(self) -> None:
        self.assertEqual(
            thinking_id("assistant", trigger().event_id),
            fixture("persona.thinking.emitted")["id"],
        )

    def test_the_suggest_id_is_the_contract_fixture_id(self) -> None:
        self.assertEqual(
            suggest_id("assistant", trigger().event_id, 1),
            fixture("persona.suggest.produced")["id"],
        )

    def test_the_natural_keys_are_the_documented_ones(self) -> None:
        # Independently of the fixtures: the key composition is what the
        # schemas describe, colon-joined and sha256-hex.
        event_id = "a" * 64
        self.assertEqual(
            thinking_id("assistant", event_id),
            hashlib.sha256(f"assistant:{event_id}".encode()).hexdigest(),
        )
        self.assertEqual(
            suggest_id("assistant", event_id, 2),
            hashlib.sha256(f"assistant:{event_id}:2".encode()).hexdigest(),
        )

    def test_an_attempt_changes_the_suggest_id_but_not_the_thinking_id(self) -> None:
        event_id = "b" * 64
        self.assertNotEqual(
            suggest_id("assistant", event_id, 1), suggest_id("assistant", event_id, 2)
        )
        self.assertNotEqual(
            thinking_id("assistant", event_id), thinking_id("watch", event_id)
        )


class ThinkingEventTest(unittest.TestCase):
    def test_it_matches_the_contract_fixture(self) -> None:
        event = thinking_event(
            persona_id="assistant",
            source=SOURCE,
            trigger=trigger(),
            model="qwen2.5-32b-instruct",
            time="2026-09-17T10:00:02Z",
        )
        self.assertEqual(event, fixture("persona.thinking.emitted"))

    def test_it_stamps_an_rfc3339_time_when_given_none(self) -> None:
        event = thinking_event(persona_id="assistant", source=SOURCE, trigger=trigger())
        self.assertRegex(event["time"], RFC3339)

    def test_it_omits_the_model_when_the_persona_names_none(self) -> None:
        event = thinking_event(persona_id="assistant", source=SOURCE, trigger=trigger())
        self.assertNotIn("model", event["data"])

    def test_it_omits_the_traceparent_when_the_trigger_carries_none(self) -> None:
        raw = fixture("inbound.message.received")
        del raw["traceparent"]
        event = thinking_event(
            persona_id="assistant", source=SOURCE, trigger=InboundMessage(raw)
        )
        self.assertNotIn("traceparent", event)

    def test_it_continues_the_trigger_trace(self) -> None:
        event = thinking_event(persona_id="assistant", source=SOURCE, trigger=trigger())
        self.assertEqual(event["traceparent"], trigger().traceparent)

    def test_it_refuses_a_trigger_with_no_id_or_no_extensions(self) -> None:
        for missing in ("id", "network", "consent"):
            raw = fixture("inbound.message.received")
            del raw[missing]
            with self.subTest(missing=missing):
                with self.assertRaises(EnvelopeError):
                    thinking_event(
                        persona_id="assistant",
                        source=SOURCE,
                        trigger=InboundMessage(raw),
                    )


class SuggestEventTest(unittest.TestCase):
    def test_it_matches_the_contract_fixture(self) -> None:
        event = suggest_event(
            persona_id="assistant",
            source=SOURCE,
            trigger=trigger(),
            suggestion=Suggestion(
                body="Pas de problème, à 20h !",
                confidence=0.86,
                rationale="Demande simple et ton amical : une confirmation courte suffit.",
                # The sentence the loop selects from the language the model
                # answered (ADR 0031) — here handed in as the loop would
                # have set it, since this module only writes it down.
                disclosure="Rédigé avec mon assistant IA.",
            ),
            time="2026-09-17T10:00:09Z",
            # The fixture's own expiry, handed in: when a suggestion goes
            # stale is the suggestion policy's decision (`twalk_sdk.policy`,
            # tested in `test_policy.py`), and this module only writes it
            # down. The fixture rounds it to the hour, as a worked example
            # may; a running persona's is exactly one window after `time`.
            expires_at="2026-09-17T11:00:00Z",
        )
        self.assertEqual(event, fixture("persona.suggest.produced"))

    def test_the_first_attempt_is_one(self) -> None:
        event = suggest_event(
            persona_id="assistant",
            source=SOURCE,
            trigger=trigger(),
            suggestion=Suggestion(body="ok"),
        )
        self.assertEqual(event["data"]["attempt"], 1)
        self.assertEqual(
            event["id"], suggest_id("assistant", trigger().event_id, 1)
        )

    def test_it_caps_the_strings_the_schema_caps(self) -> None:
        event = suggest_event(
            persona_id="assistant",
            source=SOURCE,
            trigger=trigger(),
            suggestion=Suggestion(
                body="x" * (MAX_BODY_CHARS + 100),
                rationale="y" * (MAX_RATIONALE_CHARS + 100),
            ),
        )
        self.assertEqual(len(event["data"]["suggestion"]["body"]), MAX_BODY_CHARS)
        self.assertEqual(len(event["data"]["rationale"]), MAX_RATIONALE_CHARS)

    def test_the_disclosure_is_a_field_of_its_own_and_absent_when_none_was_set(self) -> None:
        # ADR 0031: never inside the body, which the user may edit; the
        # Companion Gateway appends it at approval. And omitted rather than
        # empty when the loop set none — the schema allows the absence, and
        # the loop's own answer to "no sentence" is no suggestion at all.
        with_sentence = suggest_event(
            persona_id="assistant",
            source=SOURCE,
            trigger=trigger(),
            suggestion=Suggestion(body="ok", disclosure=SENTENCES["en"]),
        )
        self.assertEqual(with_sentence["data"]["disclosure"], SENTENCES["en"])
        self.assertEqual(with_sentence["data"]["suggestion"]["body"], "ok")
        without = suggest_event(
            persona_id="assistant",
            source=SOURCE,
            trigger=trigger(),
            suggestion=Suggestion(body="ok"),
        )
        self.assertNotIn("disclosure", without["data"])
        # And the id is the natural key's alone: a sentence does not make a
        # second suggestion of one reply.
        self.assertEqual(with_sentence["id"], without["id"])

    def test_the_body_cap_leaves_room_for_the_appended_line(self) -> None:
        # `final.body` on the approved event is capped at 65 536 and the
        # Gateway appends "\n" + disclosure to it, so the largest body the
        # SDK publishes plus the largest sentence the schema allows must fit.
        self.assertEqual(MAX_BODY_CHARS, 65_536 - 1 - DISCLOSURE_MAX_CHARS)
        self.assertLessEqual(MAX_BODY_CHARS + 1 + DISCLOSURE_MAX_CHARS, 65_536)

    def test_it_refuses_a_suggestion_the_contract_has_no_shape_for(self) -> None:
        with self.assertRaises(EnvelopeError):
            Suggestion(body="ok", format="application/pdf")
        with self.assertRaises(EnvelopeError):
            Suggestion(body="ok", confidence=1.5)
        # A sentence is selected, never written: an author cannot put their
        # own wording in the field, for the same reason they cannot forget
        # the consent gate. `language` is the member for an author who knows.
        with self.assertRaises(EnvelopeError) as refused:
            Suggestion(body="ok", disclosure="Written by a robot.")
        self.assertIn("language", str(refused.exception))
        self.assertEqual(Suggestion(body="ok", language="fr").disclosure, None)
        with self.assertRaises(EnvelopeError):
            suggest_event(
                persona_id="assistant",
                source=SOURCE,
                trigger=trigger(),
                suggestion=Suggestion(body="ok"),
                attempt=0,
            )


class NatsHeadersTest(unittest.TestCase):
    def test_they_carry_the_dedup_anchor_and_the_filtering_extensions(self) -> None:
        event = thinking_event(persona_id="assistant", source=SOURCE, trigger=trigger())
        self.assertEqual(
            nats_headers(event),
            {
                "Nats-Msg-Id": event["id"],
                "network": "whatsapp",
                "connection": "whatsapp",
                "consent": "granted",
                "traceparent": trigger().traceparent,
            },
        )

    def test_the_connection_is_copied_from_the_trigger_and_never_derived(self) -> None:
        # ADR 0033, #269: the perimeter the trigger arrived on is the one the
        # persona's answer belongs to — a second WhatsApp account is a second
        # connection, and only the trigger knows which.
        raw = fixture("inbound.message.received")
        raw["connection"] = "wa-work"
        event = thinking_event(
            persona_id="assistant", source=SOURCE, trigger=InboundMessage(raw)
        )
        self.assertEqual(event["connection"], "wa-work")
        self.assertEqual(nats_headers(event)["connection"], "wa-work")

    def test_a_trigger_that_names_no_connection_is_refused_and_never_guessed(self) -> None:
        # The bus may keep events published before #269, which carry no
        # connection. The persona does not stand the network's name in for
        # it: which perimeter that was is the registry's to say, not the
        # persona's, and an envelope stamped with a guess is one the wrong
        # decisions would govern.
        raw = fixture("inbound.message.received")
        del raw["connection"]
        with self.assertRaises(EnvelopeError) as refused:
            thinking_event(
                persona_id="assistant", source=SOURCE, trigger=InboundMessage(raw)
            )
        self.assertIn("connection", str(refused.exception))
        # And the loop terminates the delivery on the first try rather than
        # retrying it to the consumer's limit: a redelivery carries the same
        # trigger, so the answer cannot change (the rule `completion.LlmError`
        # set for a model that spent its budget reasoning).
        self.assertIs(refused.exception.transient, False)

    def test_the_traceparent_header_is_absent_when_the_event_has_none(self) -> None:
        raw = fixture("inbound.message.received")
        del raw["traceparent"]
        event = thinking_event(
            persona_id="assistant", source=SOURCE, trigger=InboundMessage(raw)
        )
        self.assertNotIn("traceparent", nats_headers(event))


if __name__ == "__main__":
    unittest.main()
