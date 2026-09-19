"""Reading a chat-completions answer, and naming which outcome it is.

Pure, and here rather than in the HTTP client, for the same reason
``LlmConfig.chat_completions_payload`` is in :mod:`twalk_sdk.config`: what an
answer *means* is a decision, it is the part of the model call worth testing
without a network, and the persona loop has to act on it (issue #162).

A call comes back as a completion or as one of four failures, and a
persona must never confuse them:

* a **completion** — text the persona may suggest;
* the endpoint **could not be reached**, or **refused** the request
  (:class:`LlmUnreachable`, :class:`LlmRefused`) — raised by the client,
  which is the only thing that sees the transport;
* the model **answered nothing** (:class:`LlmAnsweredNothing`) — it stopped
  on its own with an empty completion, or the answer is not a completion at
  all;
* the model **spent its budget thinking**
  (:class:`LlmSpentItsBudgetThinking`) — ``HTTP 200``, ``finish_reason:
  "length"``, ``content: null``, and the whole budget in
  ``reasoning_content``. A reasoning model asked for a reply-sized budget
  does this every time.

The last one is why this module exists. It answered `200`, so it is not a
failure of the endpoint; it produced no text, so it is not a completion; and
**asking again produces the same answer and another invoice**, so it is not
a transient failure either. That is what :attr:`LlmError.transient` says,
and what the persona loop reads to decide between a retry and a refusal
(:mod:`twalk_sdk.persona`).

The remedy belongs in the message rather than in a ticket: the budget is a
provider parameter, and the operator sets provider parameters in one place
(``TWALK_LLM_PARAMS`` in a persona's own environment, ``HERMES_LLM_PARAMS``
in the deployment the Hermes runtime injects it from — ADR 0015).
"""

from __future__ import annotations

from typing import Any, Mapping, Optional

#: ``finish_reason`` when the model stopped because it ran out of budget
#: rather than because it had finished.
LENGTH = "length"

#: Where an OpenAI-compatible endpoint puts a reasoning model's thinking.
#: Two spellings are in the wild — LiteLLM and DeepSeek answer
#: ``reasoning_content``, some proxies ``reasoning`` — and a persona that
#: recognised only one would report the other as "the model answered
#: nothing".
REASONING_KEYS = ("reasoning_content", "reasoning")

#: What the refusal tells the operator to do, once, in both vocabularies:
#: the variable the persona reads, and the variable the deployment sets.
BUDGET_REMEDY = (
    "raise the budget where provider parameters are set — "
    'TWALK_LLM_PARAMS={"max_tokens":2000} in this persona\'s environment, '
    'HERMES_LLM_PARAMS={"max_tokens":2000} in the deployment the Hermes '
    "runtime injects it from (ADR 0015)"
)


class LlmError(RuntimeError):
    """A completion did not happen, and this says which way.

    :attr:`transient` is the one thing the persona loop asks of it: whether
    sending the same request again could possibly answer differently. Retry
    is for a failure that might pass; against a model behaving exactly as
    configured it is only a second invoice.
    """

    #: Retried (bounded by the consumer's redelivery limit) unless a subclass
    #: says otherwise.
    transient = True


class LlmUnreachable(LlmError):
    """Nothing answered at the endpoint the operator configured."""


class LlmRefused(LlmError):
    """The endpoint answered, and it was not a completion: an HTTP error, or
    a body that is not JSON."""


class LlmAnsweredNothing(LlmError):
    """The model answered, and its answer holds no text.

    Transient: a model that stopped on its own with an empty completion may
    well answer the same prompt next time, and the number of tries is
    bounded by the consumer's redelivery limit either way.
    """


class LlmSpentItsBudgetThinking(LlmError):
    """The model reasoned until the budget was gone and never answered.

    **Not transient**, which is the whole point of naming it: the same
    request gets the same non-answer, and this one is metered. The message
    names the remedy, because it is an operator's to apply.
    """

    transient = False


def completion_text(body: Mapping[str, Any], *, budget: Optional[int] = None) -> str:
    """The assistant's text from one chat-completions answer.

    Raises :class:`LlmAnsweredNothing` or
    :class:`LlmSpentItsBudgetThinking` — never returns an empty string, and
    never invents a reply. ``budget`` is the ``max_tokens`` the request
    actually carried, so the refusal can name the number that was too small.
    """
    choices = body.get("choices")
    if not isinstance(choices, list) or not choices:
        raise LlmAnsweredNothing(
            "the chat-completions answer carries no choices, so it is not a "
            "completion at all"
        )
    choice = choices[0] if isinstance(choices[0], Mapping) else {}
    message = choice.get("message")
    message = message if isinstance(message, Mapping) else {}

    content = message.get("content")
    if isinstance(content, str) and content.strip():
        return content.strip()

    finish_reason = choice.get("finish_reason")
    reasoning = _reasoning(message)
    if finish_reason == LENGTH or reasoning:
        raise LlmSpentItsBudgetThinking(
            "the model spent its whole "
            f"{_budget(budget)} on reasoning and never answered: "
            f"finish_reason={finish_reason!r}, no content, and "
            f"{len(reasoning)} characters of reasoning_content. Asking again "
            f"produces the same answer and another invoice, so this trigger "
            f"is not retried — {BUDGET_REMEDY}."
        )
    raise LlmAnsweredNothing(
        "the model answered nothing: it stopped on its own "
        f"(finish_reason={finish_reason!r}) with an empty completion and no "
        "reasoning. The endpoint and the budget are not the problem; the "
        "model had nothing to say about this message."
    )


def _budget(budget: Optional[int]) -> str:
    """The budget as the refusal names it: the number when the request
    carried one, and the honest absence when it did not (the endpoint's own
    default was in force, and it is not Twalk's to state)."""
    if isinstance(budget, int) and not isinstance(budget, bool):
        return f"{budget}-token budget"
    return "completion budget"


def _reasoning(message: Mapping[str, Any]) -> str:
    for key in REASONING_KEYS:
        value = message.get(key)
        if isinstance(value, str) and value.strip():
            return value
    return ""
