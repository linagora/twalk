"""The suggestion policy: the expiry a draft ages out on, and the attempt
discipline that keeps a replay from becoming a second draft (issue #22).

Pure, so these run wherever Python does. What the policy does *inside a
running persona* — that every suggestion carries an expiry, that the window
is the operator's, and that a redelivered trigger produces one suggestion
and not two — is asserted at the persona's process boundary, in
`hermes/tests/suggestion.rs`.
"""

from __future__ import annotations

import re
import unittest
from datetime import datetime, timedelta, timezone

from fixtures import fixture
from twalk_sdk import (
    DEFAULT_SUGGESTION_TTL_SECONDS,
    InboundMessage,
    Suggestion,
    SuggestionPolicy,
    suggest_event,
    suggest_id,
)
from twalk_sdk.config import Config, ConfigError

RFC3339 = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")

PRODUCED_AT = datetime(2026, 9, 17, 10, 0, 9, tzinfo=timezone.utc)

SOURCE = "hermes://twalk.example.com/personas/assistant"


def trigger() -> InboundMessage:
    return InboundMessage(fixture("inbound.message.received"))


def environment(**overrides: str) -> dict:
    env = {
        "TWALK_PERSONA_ID": "assistant",
        "TWALK_HERMES_DOMAIN": "twalk.example.com",
        "TWALK_LLM_BASE_URL": "http://llm:8080/v1",
        "TWALK_LLM_MODEL": "qwen2.5-32b-instruct",
    }
    env.update(overrides)
    return env


class ExpiryTest(unittest.TestCase):
    def test_the_default_window_is_an_hour(self) -> None:
        self.assertEqual(DEFAULT_SUGGESTION_TTL_SECONDS, 3600)
        self.assertEqual(
            SuggestionPolicy().expires_at(PRODUCED_AT), "2026-09-17T11:00:09Z"
        )

    def test_it_expires_one_window_after_the_suggestion_was_produced(self) -> None:
        policy = SuggestionPolicy(ttl_seconds=900)
        self.assertEqual(policy.expires_at(PRODUCED_AT), "2026-09-17T10:15:09Z")

    def test_it_writes_the_contracts_date_time(self) -> None:
        expires_at = SuggestionPolicy().expires_at(PRODUCED_AT)
        self.assertRegex(expires_at, RFC3339)

    def test_it_answers_in_utc_whatever_zone_it_was_handed(self) -> None:
        # A container's clock may well not be UTC; the contract's `date-time`
        # here always is, so two deployments' events compare directly.
        paris = timezone(timedelta(hours=2))
        self.assertEqual(
            SuggestionPolicy().expires_at(PRODUCED_AT.astimezone(paris)),
            "2026-09-17T11:00:09Z",
        )

    def test_there_is_no_window_that_never_ends(self) -> None:
        # An approval that can be given at any later date is precisely what
        # the expiry exists to prevent, so zero and negative are refused
        # rather than read as "never expires".
        for refused in (0, -1, -3600):
            with self.subTest(ttl_seconds=refused):
                with self.assertRaises(ValueError):
                    SuggestionPolicy(ttl_seconds=refused)

    def test_a_window_is_a_whole_number_of_seconds(self) -> None:
        for refused in (3600.5, "3600", None, True):
            with self.subTest(ttl_seconds=refused):
                with self.assertRaises(ValueError):
                    SuggestionPolicy(ttl_seconds=refused)  # type: ignore[arg-type]


class ConfiguredWindowTest(unittest.TestCase):
    def test_the_window_defaults_to_an_hour(self) -> None:
        config = Config.from_env(environment())
        self.assertEqual(
            config.suggestion.ttl_seconds, DEFAULT_SUGGESTION_TTL_SECONDS
        )

    def test_the_operator_sets_the_window(self) -> None:
        config = Config.from_env(
            environment(TWALK_SUGGESTION_TTL_SECONDS="900")
        )
        self.assertEqual(config.suggestion.ttl_seconds, 900)

    def test_an_empty_value_is_the_default_rather_than_a_refusal(self) -> None:
        # Compose passes an unset variable through as an empty string, which
        # must mean "the operator said nothing" and not "zero seconds".
        config = Config.from_env(environment(TWALK_SUGGESTION_TTL_SECONDS="  "))
        self.assertEqual(
            config.suggestion.ttl_seconds, DEFAULT_SUGGESTION_TTL_SECONDS
        )

    def test_a_window_that_is_not_one_refuses_to_start_the_persona(self) -> None:
        for refused in ("an hour", "0", "-60", "3600.5"):
            with self.subTest(value=refused):
                with self.assertRaises(ConfigError) as refusal:
                    Config.from_env(
                        environment(TWALK_SUGGESTION_TTL_SECONDS=refused)
                    )
                self.assertIn("TWALK_SUGGESTION_TTL_SECONDS", str(refusal.exception))


class AttemptTest(unittest.TestCase):
    """The attempt counts suggestions that exist, not deliveries that were
    tried. The property is the deterministic id: same persona, same trigger,
    same attempt ⇒ same id, whatever else changed."""

    def test_two_productions_of_one_attempt_share_an_id_whatever_the_clock(
        self,
    ) -> None:
        events = [
            suggest_event(
                persona_id="assistant",
                source=SOURCE,
                trigger=trigger(),
                suggestion=Suggestion(body=body),
                time=time,
                expires_at=SuggestionPolicy().expires_at(produced_at),
            )
            # A redelivery redraws the clock and, with a real model, may well
            # redraw the draft too.
            for body, time, produced_at in (
                ("Pas de problème, à 20h !", "2026-09-17T10:00:09Z", PRODUCED_AT),
                ("Ça marche pour 20h.", "2026-09-17T10:04:41Z", PRODUCED_AT),
            )
        ]
        self.assertEqual(events[0]["id"], events[1]["id"])
        self.assertEqual(
            events[0]["id"], suggest_id("assistant", trigger().event_id, 1)
        )
        self.assertNotEqual(events[0]["data"]["suggestion"], events[1]["data"]["suggestion"])

    def test_a_deliberate_second_attempt_is_a_different_suggestion(self) -> None:
        second = suggest_event(
            persona_id="assistant",
            source=SOURCE,
            trigger=trigger(),
            suggestion=Suggestion(body="Ou alors 20h30, si ça t'arrange mieux ?"),
            attempt=2,
        )
        self.assertEqual(second["data"]["attempt"], 2)
        self.assertEqual(
            second["id"], suggest_id("assistant", trigger().event_id, 2)
        )
        self.assertEqual(
            second["subject"],
            trigger().event_id,
            "a second attempt still answers the same trigger",
        )


class EnvelopeExpiryTest(unittest.TestCase):
    def test_the_envelope_carries_the_policys_answer(self) -> None:
        event = suggest_event(
            persona_id="assistant",
            source=SOURCE,
            trigger=trigger(),
            suggestion=Suggestion(body="ok"),
            time="2026-09-17T10:00:09Z",
            expires_at=SuggestionPolicy().expires_at(PRODUCED_AT),
        )
        self.assertEqual(event["data"]["expires_at"], "2026-09-17T11:00:09Z")

    def test_the_envelope_builder_invents_no_expiry_of_its_own(self) -> None:
        # When a suggestion expires is a decision, and this module only knows
        # how to write one down — the loop is where the policy is applied.
        event = suggest_event(
            persona_id="assistant",
            source=SOURCE,
            trigger=trigger(),
            suggestion=Suggestion(body="ok"),
        )
        self.assertNotIn("expires_at", event["data"])


if __name__ == "__main__":
    unittest.main()
