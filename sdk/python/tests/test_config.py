"""The configuration: what an operator must set, and what a persona refuses
to start without."""

from __future__ import annotations

import unittest

from twalk_sdk import Config, ConfigError

MINIMAL = {
    "TWALK_PERSONA_ID": "assistant",
    "TWALK_HERMES_DOMAIN": "twalk.example.com",
    "TWALK_LLM_BASE_URL": "http://llm.internal:8080/v1",
    "TWALK_LLM_MODEL": "qwen2.5-32b-instruct",
}


def config(**overrides: str) -> Config:
    env = {**MINIMAL, **overrides}
    return Config.from_env({key: value for key, value in env.items() if value != ""})


class ConfigTest(unittest.TestCase):
    def test_four_variables_are_enough_to_run_a_persona(self) -> None:
        loaded = config()
        self.assertEqual(loaded.source, "hermes://twalk.example.com/personas/assistant")
        self.assertEqual(loaded.nats_url, "nats://localhost:4222")
        self.assertEqual(loaded.stream, "twalk")
        self.assertEqual(loaded.durable_name, "persona-assistant")
        self.assertEqual(
            loaded.llm.chat_completions_url, "http://llm.internal:8080/v1/chat/completions"
        )
        self.assertIsNone(loaded.llm.api_key)
        self.assertEqual(loaded.llm.params, {})

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

    def test_the_endpoint_and_the_model_are_both_required(self) -> None:
        # Twalk ships no LLM and names no model of its own: a persona
        # refuses to start rather than send a message somewhere the
        # operator did not choose (ADR 0015).
        with self.assertRaises(ConfigError):
            config(TWALK_LLM_BASE_URL="")
        with self.assertRaises(ConfigError):
            config(TWALK_LLM_MODEL="")

    def test_a_non_numeric_timeout_fails_at_startup(self) -> None:
        with self.assertRaises(ConfigError):
            config(TWALK_LLM_TIMEOUT_SECONDS="soon")

    def test_the_consumer_name_can_be_overridden(self) -> None:
        self.assertEqual(
            config(TWALK_PERSONA_CONSUMER="persona-assistant-run7").durable_name,
            "persona-assistant-run7",
        )


class ProviderParametersTest(unittest.TestCase):
    """ADR 0015's passthrough: providers differ in what they reject, so the
    operator's parameters reach the endpoint untouched — and can remove a
    field this client would otherwise send."""

    def payload(self, params: str = "") -> dict:
        loaded = config(TWALK_LLM_PARAMS=params)
        return loaded.llm.chat_completions_payload(
            [{"role": "user", "content": "on décale à 20h ?"}],
            temperature=0.2,
            max_tokens=300,
        )

    def test_the_payload_names_the_model_and_carries_the_conversation(self) -> None:
        payload = self.payload()
        self.assertEqual(payload["model"], "qwen2.5-32b-instruct")
        self.assertEqual(
            payload["messages"], [{"role": "user", "content": "on décale à 20h ?"}]
        )
        self.assertEqual(payload["temperature"], 0.2)
        self.assertEqual(payload["max_tokens"], 300)

    def test_a_parameter_is_passed_through_untouched(self) -> None:
        payload = self.payload('{"top_p": 0.9, "provider": {"order": ["ovh"]}}')
        self.assertEqual(payload["top_p"], 0.9)
        self.assertEqual(
            payload["provider"],
            {"order": ["ovh"]},
            "a nested object reaches the endpoint as the operator wrote it",
        )

    def test_a_null_parameter_removes_a_field_the_provider_rejects(self) -> None:
        payload = self.payload('{"temperature": null, "max_tokens": null}')
        self.assertNotIn("temperature", payload)
        self.assertNotIn("max_tokens", payload)
        self.assertIn("messages", payload, "only the named fields are dropped")

    def test_a_parameter_overrides_what_the_persona_asked_for(self) -> None:
        # The operator's endpoint has the last word: the persona's sampling
        # is a default, not a requirement.
        self.assertEqual(self.payload('{"temperature": 1}')["temperature"], 1)

    def test_parameters_that_are_not_a_json_object_fail_at_startup(self) -> None:
        for params in ("not json", "[1, 2]", '"a string"', "42"):
            with self.subTest(params=params):
                with self.assertRaises(ConfigError):
                    config(TWALK_LLM_PARAMS=params)


if __name__ == "__main__":
    unittest.main()
