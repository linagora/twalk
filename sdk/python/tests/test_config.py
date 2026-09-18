"""The configuration: what an operator must set, and what a persona refuses
to start without."""

from __future__ import annotations

import unittest

from twalk_sdk import Config, ConfigError

MINIMAL = {
    "TWALK_PERSONA_ID": "assistant",
    "TWALK_HERMES_DOMAIN": "twalk.example.com",
    "TWALK_LLM_BASE_URL": "http://llm.internal:8080/v1",
}


def config(**overrides: str) -> Config:
    env = {**MINIMAL, **overrides}
    return Config.from_env({key: value for key, value in env.items() if value != ""})


class ConfigTest(unittest.TestCase):
    def test_three_variables_are_enough_to_run_a_persona(self) -> None:
        loaded = config()
        self.assertEqual(loaded.source, "hermes://twalk.example.com/personas/assistant")
        self.assertEqual(loaded.nats_url, "nats://localhost:4222")
        self.assertEqual(loaded.stream, "twalk")
        self.assertEqual(loaded.durable_name, "persona-assistant")
        self.assertEqual(
            loaded.llm.chat_completions_url, "http://llm.internal:8080/v1/chat/completions"
        )
        self.assertIsNone(loaded.llm.api_key)

    def test_it_maps_a_contract_event_type_to_its_bus_subject(self) -> None:
        loaded = config()
        self.assertEqual(
            loaded.subject("fr.linagora.twalk.inbound.message.received.v1"),
            "twalk.inbound.message.received.v1",
        )
        self.assertEqual(
            config(TWALK_BUS_SUBJECT_PREFIX="twalk-test").subject(
                "fr.linagora.twalk.persona.suggest.produced.v1"
            ),
            "twalk-test.persona.suggest.produced.v1",
        )
        with self.assertRaises(ValueError):
            loaded.subject("com.example.something.else.v1")

    def test_a_persona_id_the_contract_would_refuse_fails_at_startup(self) -> None:
        for persona_id in ("Assistant", "my persona", "assistant/2", ""):
            with self.subTest(persona_id=persona_id):
                with self.assertRaises(ConfigError):
                    config(TWALK_PERSONA_ID=persona_id)

    def test_a_domain_with_a_slash_fails_at_startup(self) -> None:
        # It is the authority of the source URI: a slash would produce a
        # source no schema accepts.
        with self.assertRaises(ConfigError):
            config(TWALK_HERMES_DOMAIN="twalk.example.com/hermes")

    def test_the_endpoint_is_required(self) -> None:
        with self.assertRaises(ConfigError):
            config(TWALK_LLM_BASE_URL="")

    def test_a_non_numeric_timeout_fails_at_startup(self) -> None:
        with self.assertRaises(ConfigError):
            config(TWALK_LLM_TIMEOUT_SECONDS="soon")

    def test_the_consumer_name_can_be_overridden(self) -> None:
        self.assertEqual(
            config(TWALK_PERSONA_CONSUMER="persona-assistant-run7").durable_name,
            "persona-assistant-run7",
        )


if __name__ == "__main__":
    unittest.main()
