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
"""

from __future__ import annotations

from typing import Dict, Optional

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

#: The one answer that is not a tag: the model saying the reply is in none
#: of the five. Read as "no language", so the refusal names what the model
#: said rather than inventing a tag it did not.
OTHER = "other"

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
            f"disclosure has no sentence for the language {who}: {tag or OTHER}. "
            f"A reply the contact cannot be told was drafted with an AI assistant "
            f"is no suggestion at all (ADR 0031); the languages with a sentence "
            f"are {', '.join(SENTENCES)}, from contracts/disclosure/v1/sentences.json"
        )


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
