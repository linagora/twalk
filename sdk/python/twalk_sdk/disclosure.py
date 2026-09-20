"""The sentence a persona's reply carries to the contact, and how the persona
picks it (ADR 0019, ADR 0031, issue #121).

Every reply a persona drafted reaches the contact with one sentence after
it, on a line of its own, in the language the reply was written in:
*"Rédigé avec mon assistant IA."*, *"Drafted with my AI assistant."*. The
sentence is **not** written by the model — a model can be argued out of
anything — and not composed by the Companion Gateway, which knows no
language and holds no catalogue. The persona **selects** it, because the
persona is the only component that knows what language it wrote in, and it
travels as a field of its own (``data.disclosure``) rather than inside the
body the user may edit. The Gateway appends it at approval.

Three decisions live here rather than in a reader's head.

**The sentences are the contract's**, not this module's:
``contracts/disclosure/v1/sentences.json`` is the authority, and
:data:`SENTENCES` is a copy of it that ``tests/test_disclosure.py`` pins to
the file value for value — the Gateway reads the same file, so the two
components cannot disagree about what a contact is told. A sentence changes
only by a new version directory of the contract, never by an edit here.

**The language is the model's answer, never a library's guess** (ADR 0016:
detection is the model's job). After a persona's handler returns a
suggestion, the SDK asks the same model one more question — the text, and
:data:`LANGUAGE_ASK` as the system prompt — and reads one token back. An
author who *knows* the language (a persona that only ever writes French)
says so on the :class:`~twalk_sdk.envelope.Suggestion` and skips the ask.

**A language with no sentence is no suggestion at all.** ``sentence_for``
answers ``None``, and the persona loop raises :class:`DisclosureError`,
which is **not transient**: the model would answer the same language to the
same reply, so the trigger is terminated on the first delivery with one
``ERROR`` line naming the tag, exactly as a model that spent its whole
budget reasoning is (:mod:`twalk_sdk.completion`). Falling back to English,
or to the user's own language, was rejected in ADR 0031: a disclosure the
contact cannot read is a sentence nobody reads, and a named refusal is a
contribution request an operator can act on with one line.

**The ask has a budget, and a reasoning model needs it raised.** The ask
carries :data:`LANGUAGE_ASK_MAX_TOKENS` — five, because one tag is all it
needs back — unless the operator's provider parameters name ``max_tokens``,
which are merged last and then govern the ask exactly as they govern the
reply. A reasoning model spends its first tokens thinking, so with five it
answers nothing and :mod:`twalk_sdk.completion` reports it as having spent
its whole budget reasoning — #162's failure again, one call later and after
the reply was already drafted and billed. The loop wraps that in
:class:`LanguageAskFailed` so the ``ERROR`` line says which of the two calls
it was and names the remedy: ``TWALK_LLM_PARAMS={"max_tokens": N}`` (from
``HERMES_LLM_PARAMS`` in a deployment), which is documented as required on
a reasoning model rather than left for an operator to infer from a log.
"""

from __future__ import annotations

from typing import Dict, Optional

from .completion import BUDGET_REMEDY, LlmError, LlmSpentItsBudgetThinking

#: The contract's five sentences, keyed by the primary language subtag. A
#: copy of ``contracts/disclosure/v1/sentences.json``, pinned to it by
#: ``tests/test_disclosure.py``: if the two disagree, the contract wins and
#: the test says so.
SENTENCES: Dict[str, str] = {
    "en": "Drafted with my AI assistant.",
    "fr": "Rédigé avec mon assistant IA.",
    "it": "Scritto con il mio assistente IA.",
    "es": "Redactado con mi asistente de IA.",
    "de": "Verfasst mit meinem KI-Assistenten.",
}

#: The schema's cap on ``data.disclosure``. Named here because
#: :mod:`twalk_sdk.envelope` subtracts it, plus the newline, from the body's
#: own cap, so that body + line never exceeds ``final.body``'s 65 536 when
#: the Companion Gateway appends the sentence at approval.
DISCLOSURE_MAX_CHARS = 200

#: What the persona asks its model after the reply is drafted, verbatim: one
#: system prompt, the reply as the user message, one token back. The
#: wording is load-bearing beyond this package — the test harness's stub
#: model recognises the ask by the phrase "exactly one language tag" — so it
#: is a constant and not a sentence composed at the call site.
LANGUAGE_ASK = "Answer with exactly one language tag among en, fr, it, es, de, or other."

#: The ``max_tokens`` the ask carries when the operator's provider
#: parameters name none: one tag is all it needs back. Provider parameters
#: are merged last (:mod:`twalk_sdk.config`), so ``TWALK_LLM_PARAMS``
#: naming ``max_tokens`` replaces this for the ask as it does for the reply
#: — which is what a reasoning model needs, since it thinks before it
#: answers and five tokens of thinking is no answer.
LANGUAGE_ASK_MAX_TOKENS = 5

#: The one answer that is not a tag: the model saying the reply is in none
#: of the five. Read as "no language", so the refusal names what the model
#: said rather than inventing a tag it did not.
OTHER = "other"

#: How much of the model's first token a refusal echoes. The token is what
#: the model answered to a one-word question, and a model that ignored the
#: system prompt and echoed or translated the draft puts the first word of
#: the persona's reply there — not contact text, but message content, so
#: the log line carries at most this much of it.
TAG_ECHO_MAX_CHARS = 32

#: What is trimmed off the model's first token before it is read as a tag:
#: a model asked for one word still likes to end it with a period or wrap
#: it in quotes or backticks.
_PUNCTUATION = ".,;:!?\"'`*()[]{}<>"


class DisclosureError(RuntimeError):
    """The reply is in a language the contract has no sentence for, so there
    is no suggestion.

    **Not transient**, and the persona loop reads exactly that: the model
    answered which language it wrote in, and asking it again about the same
    reply returns the same language. One ``ERROR`` line, the delivery
    terminated, never a retry — the shape #162 set for a model that spent
    its whole budget reasoning, because here too every attempt is billed.
    """

    transient = False

    def __init__(self, tag: Optional[str], *, declared_by_persona: bool = False) -> None:
        self.tag = tag
        who = "the persona declared" if declared_by_persona else "the model answered"
        super().__init__(
            f"disclosure has no sentence for the language {who}: {_echo(tag)}. "
            f"A reply the contact cannot be told was drafted with an AI assistant "
            f"is no suggestion at all (ADR 0031); the languages with a sentence "
            f"are {', '.join(SENTENCES)}, from contracts/disclosure/v1/sentences.json"
        )


class LanguageAskFailed(LlmError):
    """The language ask — not the reply — is the completion that failed.

    The ask is a second call to the same model, made after the reply was
    drafted, and it fails the same four ways the reply can
    (:mod:`twalk_sdk.completion`). Reported bare, its ``ERROR`` line reads
    exactly like the reply's own failure, and an operator on a reasoning
    model — where the ask's five-token budget is spent thinking before a tag
    is written, every time — would look for a reply that was in fact drafted
    fine. So the loop wraps the cause: the line says it was the ask, says
    the reply was drafted, and names the remedy, which is the same
    ``max_tokens`` the reply's own budget lives in.

    Whether it is retried is the cause's decision, not this wrapper's:
    :attr:`transient` is copied from it, so a model that spent its budget
    thinking about a one-word question terminates the delivery the way it
    does about the reply, and an endpoint that was briefly away is retried.
    """

    def __init__(self, cause: LlmError) -> None:
        self.cause = cause
        self.transient = cause.transient
        if isinstance(cause, LlmSpentItsBudgetThinking):
            what = "exhausted the model's budget"
        else:
            what = "did not get an answer"
        super().__init__(
            f"the language ask {what}: the one-word question the SDK puts to the "
            f"model after the reply is drafted, to select the disclosure's "
            f"language (ADR 0031) — the reply itself was drafted and is not the "
            f"problem. The ask carries {LANGUAGE_ASK_MAX_TOKENS} tokens unless "
            f"TWALK_LLM_PARAMS names max_tokens, which then governs the ask as "
            f"it governs the reply; a reasoning model spends its first tokens "
            f"thinking, so on one that budget must be set — {BUDGET_REMEDY}. "
            f"The ask failed with: {cause}"
        )


def _echo(tag: Optional[str]) -> str:
    """What a refusal names as the language: the tag, at most
    :data:`TAG_ECHO_MAX_CHARS` of it, and ``other`` for none."""
    if not tag:
        return OTHER
    if len(tag) <= TAG_ECHO_MAX_CHARS:
        return tag
    return tag[:TAG_ECHO_MAX_CHARS] + "…"


def primary_subtag(tag: str) -> str:
    """The language part of a BCP 47 tag: ``fr-CA`` and ``fr_CA`` are
    ``fr``; ``FR`` is ``fr``."""
    return tag.strip().lower().replace("_", "-").split("-", 1)[0]


def sentence_for(tag: Optional[str]) -> Optional[str]:
    """The sentence for a language, or ``None`` when the contract has none.

    Keyed on the primary subtag, so a model or an author saying ``fr-CA``
    gets the French sentence: the sentence discloses a fact, and the fact is
    the same in Québec. ``None`` in, ``None`` out — a model that answered
    ``other`` declared no language.
    """
    if not tag:
        return None
    return SENTENCES.get(primary_subtag(tag))


def parse_language_answer(answer: str) -> Optional[str]:
    """The tag in a model's answer to :data:`LANGUAGE_ASK`, or ``None``.

    Lower-cased, trimmed, the first whitespace-separated token with its
    surrounding punctuation removed: ``" FR\\n"`` is ``fr``, ``"fr."`` is
    ``fr``, ``"fr-CA"`` is ``fr-ca`` (mapped to the sentence by
    :func:`sentence_for`). ``other`` and an empty answer are ``None``.
    Anything else is returned as the model said it — ``ja``, or a word that
    is not a tag at all — so that the refusal it leads to names the answer
    rather than a guess about it.
    """
    tokens = answer.strip().lower().split()
    if not tokens:
        return None
    token = tokens[0].strip(_PUNCTUATION)
    if not token or token == OTHER:
        return None
    return token
