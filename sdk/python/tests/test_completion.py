"""What a chat-completions answer means, and which of them is worth asking
again (issue #162).

Four outcomes, three of them failures, and the one this file exists for is
the fourth: a reasoning model that spent the whole budget thinking. It comes
back as `HTTP 200`, so it is not a transport failure; it carries no content,
so it is not a completion; and it will come back identically to the same
request, so it is not transient. That last property is the one the persona
loop reads, and getting it wrong costs the operator money — which is why it
is asserted here and not only in the container test.

The bodies below are the shapes a real endpoint answered with: a Qwen model
behind a LiteLLM proxy on this project's reference host.
"""

from __future__ import annotations

import unittest

from twalk_sdk import (
    LlmAnsweredNothing,
    LlmError,
    LlmSpentItsBudgetThinking,
    completion_text,
)


def answer(message: dict, finish_reason: str = "stop") -> dict:
    return {
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": "qwen",
        "choices": [{"index": 0, "message": message, "finish_reason": finish_reason}],
    }


class CompletionTest(unittest.TestCase):
    def test_a_completion_is_the_assistants_text_trimmed(self) -> None:
        text = completion_text(
            answer({"role": "assistant", "content": "  Pas de souci, à 20h !\n"})
        )
        self.assertEqual(text, "Pas de souci, à 20h !")

    def test_a_budget_spent_reasoning_is_its_own_outcome_and_is_not_retried(self) -> None:
        # The exact shape from the reference deployment: HTTP 200, the whole
        # budget in reasoning_content, and no content at all.
        body = answer(
            {
                "role": "assistant",
                "content": None,
                "reasoning_content": "The user wrote 'test'. " * 40,
            },
            finish_reason="length",
        )
        with self.assertRaises(LlmSpentItsBudgetThinking) as raised:
            completion_text(body, budget=300)
        refusal = str(raised.exception)

        self.assertFalse(
            raised.exception.transient,
            "asking again produces the same answer and another invoice, so this "
            "one must not be retried",
        )
        self.assertIn("300-token budget", refusal, "the refusal names the budget that was too small")
        self.assertIn("'length'", refusal, "and the finish_reason that says why")
        self.assertIn(
            "TWALK_LLM_PARAMS",
            refusal,
            "the remedy is an operator's to apply, so the refusal names where the "
            "budget is set — in the persona's own vocabulary",
        )
        self.assertIn(
            "HERMES_LLM_PARAMS",
            refusal,
            "and in the deployment's, which is where an operator actually types it",
        )

    def test_reasoning_with_no_budget_named_still_says_the_budget_was_the_problem(self) -> None:
        # Some proxies report `reasoning` rather than `reasoning_content`, and
        # stop with `finish_reason: "stop"` having answered nothing. Same
        # outcome, same remedy: a persona that recognised one spelling would
        # report the other as "the model answered nothing".
        body = answer({"role": "assistant", "content": "", "reasoning": "thinking…"})
        with self.assertRaises(LlmSpentItsBudgetThinking) as raised:
            completion_text(body)
        self.assertIn(
            "completion budget",
            str(raised.exception),
            "with no max_tokens in the request, the refusal names no number it "
            "cannot know",
        )

    def test_a_model_that_answered_nothing_is_a_different_outcome_and_is_retried(self) -> None:
        body = answer({"role": "assistant", "content": ""})
        with self.assertRaises(LlmAnsweredNothing) as raised:
            completion_text(body)
        self.assertTrue(
            raised.exception.transient,
            "a model that stopped on its own may answer the same prompt next "
            "time; the redelivery limit bounds it either way",
        )
        self.assertNotIn(
            "TWALK_LLM_PARAMS",
            str(raised.exception),
            "the budget is not the problem here, and a message that names the "
            "wrong remedy is worse than one that names none",
        )

    def test_an_answer_that_is_not_a_completion_is_not_a_completion(self) -> None:
        for body in ({}, {"choices": []}, {"choices": "nonsense"}):
            with self.subTest(body=body):
                with self.assertRaises(LlmAnsweredNothing):
                    completion_text(body)

    def test_every_outcome_is_an_llm_error_so_one_except_still_catches_them(self) -> None:
        # The class was `LlmError` before #162 split it: a persona author's
        # `except LlmError` must keep working.
        for body in ({}, answer({"content": None}, finish_reason="length")):
            with self.subTest(body=body):
                with self.assertRaises(LlmError):
                    completion_text(body)

    def test_a_truncated_answer_that_still_carries_text_is_a_completion(self) -> None:
        # `finish_reason: "length"` with content is a reply that ran long, not
        # a budget spent thinking. The user edits before sending; an invented
        # refusal would lose a usable draft.
        text = completion_text(
            answer({"role": "assistant", "content": "On se voit à 20h et je"}, "length"),
            budget=300,
        )
        self.assertEqual(text, "On se voit à 20h et je")


if __name__ == "__main__":
    unittest.main()
