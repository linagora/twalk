"""The disclosure: the contract's sentences, the language that selects one,
and the refusal when none does (ADR 0019, ADR 0031, issue #121).

The sentences are pinned to `contracts/disclosure/v1/sentences.json` value
for value rather than asserted from a hand-written copy: the Companion
Gateway reads that file, this SDK carries a copy, and a contact must be
told the same sentence whichever path the suggestion took.
"""

from __future__ import annotations

import json
import unittest
from pathlib import Path

from fixtures import CONTRACT_DIR
from twalk_sdk import (
    DISCLOSURE_MAX_CHARS,
    LANGUAGE_ASK,
    LANGUAGES,
    SENTENCES,
    DisclosureError,
    parse_language_answer,
    sentence_for,
)

SENTENCES_FILE = (
    Path(__file__).resolve().parents[3] / "contracts" / "disclosure" / "v1" / "sentences.json"
)


class SentencesTest(unittest.TestCase):
    def test_the_sentences_are_the_contracts_value_for_value(self) -> None:
        # The contract wins: a sentence changes only by a new version
        # directory there, never by an edit here, and this is what notices.
        contract = json.loads(SENTENCES_FILE.read_text("utf-8"))
        self.assertEqual(SENTENCES, contract)
        self.assertEqual(list(SENTENCES), list(contract), "same order too")

    def test_the_languages_with_a_sentence_are_the_companions_five(self) -> None:
        self.assertEqual(tuple(SENTENCES), LANGUAGES)

    def test_every_sentence_fits_the_schemas_bounds(self) -> None:
        schema = json.loads(
            (CONTRACT_DIR / "persona.suggest.produced.schema.json").read_text("utf-8")
        )
        member = schema["properties"]["data"]["properties"]["disclosure"]
        self.assertEqual(DISCLOSURE_MAX_CHARS, member["maxLength"])
        for tag, sentence in SENTENCES.items():
            with self.subTest(tag=tag):
                self.assertGreaterEqual(len(sentence), member["minLength"])
                self.assertLessEqual(len(sentence), DISCLOSURE_MAX_CHARS)
                # One line of its own, after the message: a newline inside
                # the sentence would be a second line the Gateway did not
                # append.
                self.assertNotIn("\n", sentence)
                self.assertEqual(sentence, sentence.strip())


class SentenceForTest(unittest.TestCase):
    def test_a_tag_of_the_five_selects_its_sentence(self) -> None:
        self.assertEqual(sentence_for("fr"), "Rédigé avec mon assistant IA.")
        self.assertEqual(sentence_for("en"), "Drafted with my AI assistant.")

    def test_a_region_maps_to_its_primary_subtag(self) -> None:
        # The sentence discloses a fact, and the fact is the same in Québec.
        for regional in ("fr-CA", "fr-ca", "fr_CA", "FR", " fr "):
            with self.subTest(tag=regional):
                self.assertEqual(sentence_for(regional), SENTENCES["fr"])
        self.assertEqual(sentence_for("de-AT"), SENTENCES["de"])
        self.assertEqual(sentence_for("es-419"), SENTENCES["es"])

    def test_a_language_with_no_sentence_is_none_and_never_a_fallback(self) -> None:
        # No "probably English": ADR 0031 has a reply that cannot be
        # disclosed produce no suggestion at all.
        for outside in ("ja", "pt", "pt-BR", "other", "", None, "french"):
            with self.subTest(tag=outside):
                self.assertIsNone(sentence_for(outside))


class LanguageAnswerTest(unittest.TestCase):
    def test_the_ask_is_the_sentence_the_stub_model_recognises(self) -> None:
        # The test harness's stub model answers the ask by matching this
        # phrase in the system prompt (hermes/tests, via
        # tests/harness/src/stub_llm.rs): the wording is shared across a
        # language boundary and cannot drift on this side unnoticed.
        self.assertIn("exactly one language tag", LANGUAGE_ASK)
        for tag in LANGUAGES:
            self.assertIn(tag, LANGUAGE_ASK)
        self.assertIn("other", LANGUAGE_ASK)

    def test_the_first_token_lower_cased_and_trimmed_is_the_tag(self) -> None:
        for answered, tag in (
            (" FR\n", "fr"),
            ("fr", "fr"),
            ("fr.", "fr"),
            ("`fr`", "fr"),
            ('"en"', "en"),
            ("De\n\nThe text is German.", "de"),
            ("fr-CA", "fr-ca"),
            ("it (Italian)", "it"),
        ):
            with self.subTest(answered=answered):
                self.assertEqual(parse_language_answer(answered), tag)

    def test_other_and_nothing_are_no_language(self) -> None:
        for answered in ("other", "Other.", " OTHER \n", "", "   ", "..."):
            with self.subTest(answered=answered):
                self.assertIsNone(parse_language_answer(answered))

    def test_an_answer_outside_the_five_is_kept_so_the_refusal_can_name_it(self) -> None:
        # The parser does not judge; `sentence_for` does. What the model
        # said — `ja`, or a word that is no tag — is what the ERROR line
        # names, so an operator reads the answer and not a guess about it.
        self.assertEqual(parse_language_answer("ja"), "ja")
        self.assertEqual(parse_language_answer("Japanese"), "japanese")
        self.assertIsNone(sentence_for(parse_language_answer("ja")))


class DisclosureErrorTest(unittest.TestCase):
    def test_it_is_not_transient_and_names_the_language_the_model_answered(self) -> None:
        # The loop terminates the delivery on the first try rather than
        # retrying it to the consumer's limit: the model would answer the
        # same language about the same reply, and every attempt is billed —
        # the rule `completion.LlmSpentItsBudgetThinking` set.
        error = DisclosureError("ja")
        self.assertIs(error.transient, False)
        self.assertEqual(error.tag, "ja")
        message = str(error)
        self.assertTrue(
            message.startswith(
                "disclosure has no sentence for the language the model answered: ja"
            ),
            message,
        )
        for tag in LANGUAGES:
            self.assertIn(tag, message)
        self.assertIn("contracts/disclosure/v1/sentences.json", message)

    def test_no_language_at_all_is_named_as_the_models_other(self) -> None:
        self.assertIn(
            "the language the model answered: other", str(DisclosureError(None))
        )

    def test_a_language_the_persona_declared_is_named_as_the_personas(self) -> None:
        message = str(DisclosureError("pt", declared_by_persona=True))
        self.assertIn("the language the persona declared: pt", message)
        self.assertNotIn("the model answered", message)


if __name__ == "__main__":
    unittest.main()
