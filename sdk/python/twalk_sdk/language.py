"""The user's own language: the one thing a persona falls back to when it
cannot tell what language it is answering (ADR 0016, issue #164).

A suggestion is written in the language of the **message** it answers — a
French user answering an English-speaking contact must not be handed French,
which is the mistake an i18n habit produces and the reason ADR 0016 exists.
The user's own language matters for exactly one case, and it is a common one
on the conversations Twalk observes: a message too short to tell. ``test``,
``ok``, ``👍``, a bare link. The first real suggestion the reference
deployment produced answered *"Test received."* to a French speaker who had
written ``test``.

So the preference is a **stored setting**, not an inference: a persona runs
in a container and has no ``navigator.language`` to read. The Companion sets
it, the Companion Gateway holds it, and the Hermes runtime injects it into
each persona's environment beside the model configuration (ADR 0015) —
``TWALK_USER_LANGUAGE`` here, ``HERMES_USER_LANGUAGE`` on the runtime.

Two decisions are recorded here rather than left to a reader.

**The five are a closed list**, the interface languages the Companion ships
(``companion/src/lib/i18n/``, ``CONTRIBUTING.md``), and the same five the
Gateway stores. A tag outside them is a configuration error and is refused
at startup with the five named — not stored and discovered later by a
persona that has no name for it.

**Unset is not a refusal to start.** A persona with no preference writes in
the language of the message and nothing else; on a message it cannot read
the language of, the model chooses. That is a real gap and the SDK says so
in the log at startup rather than deciding silently — which is the part of
this that was actually wrong before #164, not the absence of a default.
Refusing to start was the alternative and was rejected: it would take a
deployment that suggests replies in every unambiguous case and stop it dead
over the ambiguous ones, and it would make a preference the user has not
opened the settings screen for into an outage. No default is invented: there
is no "probably English".
"""

from __future__ import annotations

from typing import Dict, Optional

#: The five, and the English name each is written into a prompt as. Prompts
#: stay in English (ADR 0016), so these names are prompt text and not
#: interface copy: nothing here is ever shown to a user.
LANGUAGE_NAMES: Dict[str, str] = {
    "en": "English",
    "fr": "French",
    "it": "Italian",
    "es": "Spanish",
    "de": "German",
}

#: The tags, in the order the Companion offers them.
LANGUAGES = tuple(LANGUAGE_NAMES)

#: The variable the runtime injects the preference through, named here so
#: that a message about it cannot drift from the thing it names.
USER_LANGUAGE_VARIABLE = "TWALK_USER_LANGUAGE"


def is_language(tag: str) -> bool:
    """Whether ``tag`` is one of the five, spelled the way the Companion
    writes it: ``fr``, never ``fr-FR`` and never ``FR``."""
    return tag in LANGUAGE_NAMES


def language_name(tag: str) -> str:
    """The English name of a language tag, for a prompt.

    Raises :class:`KeyError` on anything else: the configuration is
    validated at startup, so an unknown tag reaching here is a bug rather
    than an operator's mistake.
    """
    return LANGUAGE_NAMES[tag]


def available() -> str:
    """The five, as a refusal lists them."""
    return ", ".join(LANGUAGES)


def parse(tag: Optional[str]) -> Optional[str]:
    """The preference as the SDK holds it: one of the five, or ``None``.

    An empty value is ``None`` — an empty variable is how a compose file
    passes on "the operator set nothing".
    """
    tag = (tag or "").strip()
    if not tag:
        return None
    if not is_language(tag):
        raise ValueError(
            f"{USER_LANGUAGE_VARIABLE} is the user's own language, one of "
            f"{available()} — the interface languages the Companion ships and "
            f"the Companion Gateway stores (ADR 0016); got {tag!r}"
        )
    return tag
