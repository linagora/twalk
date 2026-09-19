"""The user's own language: the value ADR 0016's fallback needs, and what a
persona does without it (issue #164)."""

from __future__ import annotations

import unittest

from twalk_sdk import Config, ConfigError, LANGUAGES, language_name
from twalk_sdk.language import parse

MINIMAL = {
    "TWALK_PERSONA_ID": "assistant",
    "TWALK_HERMES_DOMAIN": "twalk.example.com",
    "TWALK_LLM_BASE_URL": "http://llm.internal:8080/v1",
    "TWALK_LLM_MODEL": "qwen2.5-32b-instruct",
}


def config(**overrides: str) -> Config:
    return Config.from_env({**MINIMAL, **overrides})


class LanguageTest(unittest.TestCase):
    def test_the_preference_is_read_from_the_environment(self) -> None:
        self.assertEqual(config(TWALK_USER_LANGUAGE="fr").user_language, "fr")

    def test_unset_is_a_supported_state_and_not_a_default(self) -> None:
        # A persona with no preference starts and answers every message whose
        # language it can read. What it must not do is invent "probably
        # English" — `user_language` is None, and the loop says so in its log.
        for unset in ({}, {"TWALK_USER_LANGUAGE": ""}, {"TWALK_USER_LANGUAGE": "  "}):
            with self.subTest(env=unset):
                self.assertIsNone(config(**unset).user_language)

    def test_a_tag_outside_the_five_fails_at_startup_with_the_five_named(self) -> None:
        # The Companion Gateway refuses one on the way in, so a persona that
        # saw it was configured by hand. Storing it and discovering it at the
        # first ambiguous message would be worse.
        for refused in ("fr-FR", "FR", "pt", "français", "en,fr"):
            with self.subTest(tag=refused):
                with self.assertRaises(ConfigError) as raised:
                    config(TWALK_USER_LANGUAGE=refused)
                message = str(raised.exception)
                self.assertIn("TWALK_USER_LANGUAGE", message)
                for language in LANGUAGES:
                    self.assertIn(language, message)

    def test_the_five_are_the_companions_five(self) -> None:
        self.assertEqual(LANGUAGES, ("en", "fr", "it", "es", "de"))

    def test_a_prompt_gets_the_languages_english_name(self) -> None:
        # Prompts stay in English (ADR 0016), so what goes into one is
        # "French" and never "français" — and never the bare tag, which a
        # model may or may not read as a language.
        self.assertEqual(language_name("fr"), "French")
        self.assertEqual(
            [language_name(tag) for tag in LANGUAGES],
            ["English", "French", "Italian", "Spanish", "German"],
        )
        with self.assertRaises(KeyError):
            language_name("pt")

    def test_parse_is_the_one_place_a_tag_is_judged(self) -> None:
        self.assertEqual(parse("de"), "de")
        self.assertIsNone(parse(None))
        with self.assertRaises(ValueError):
            parse("de-AT")


if __name__ == "__main__":
    unittest.main()
