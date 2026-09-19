"""twalk_sdk — write a Twalk persona without touching NATS or CloudEvents.

A persona is a process that consumes inbound events from the bus and
publishes thinking and suggestion events back onto it. This SDK is the part
of that which is the same for every persona:

* the **consent gate** (:mod:`twalk_sdk.consent`), applied by the SDK
  before any persona code runs, so that an author cannot breach consent by
  forgetting;
* the **trigger-type gate** (:func:`twalk_sdk.trigger.triggers_a_persona`),
  beside it and for the same reason: the user's own messages are on the bus
  (ADR 0018) and no persona is woken by them;
* the contract's envelopes and their deterministic ids
  (:mod:`twalk_sdk.envelope`);
* the suggestion policy — the expiry a draft ages out on, and the attempt
  discipline that keeps a replay from becoming a second draft
  (:mod:`twalk_sdk.policy`);
* the durable subscription, the publishing and the process loop
  (:mod:`twalk_sdk.persona`);
* an OpenAI-compatible chat-completions client (:mod:`twalk_sdk.llm`), and
  what its answers mean (:mod:`twalk_sdk.completion`) — including the one a
  reasoning model gives when it spends its whole budget thinking, which is
  named, is not retried, and names its own remedy (issue #162);
* the user's own language (:mod:`twalk_sdk.language`), which a persona falls
  back to only when it cannot tell what language it is answering
  (ADR 0016).

The reference persona built on it is ``hermes/personas/assistant/``.

Nothing in this package talks to Matrix: a persona sees the bus and the
model it was pointed at, and nothing else.

The package's pure half — the gate, the envelopes, the configuration —
imports nothing outside the standard library, and the two halves that need
a dependency (``nats-py``, ``httpx``) are imported on first use. That is
what lets the gate's own tests run wherever Python does, while the persona
itself only ever runs from its container image.
"""

from __future__ import annotations

from typing import Any

from .completion import (
    LlmAnsweredNothing,
    LlmError,
    LlmRefused,
    LlmSpentItsBudgetThinking,
    LlmUnreachable,
    completion_text,
)
from .config import Config, ConfigError, LlmConfig
from .consent import GRANTED, consent_of, is_granted
from .envelope import (
    FIRST_ATTEMPT,
    SUGGEST_TYPE,
    THINKING_TYPE,
    EnvelopeError,
    Suggestion,
    deterministic_id,
    nats_headers,
    suggest_event,
    suggest_id,
    thinking_event,
    thinking_id,
)
from .language import (
    LANGUAGE_NAMES,
    LANGUAGES,
    USER_LANGUAGE_VARIABLE,
    language_name,
)
from .policy import DEFAULT_SUGGESTION_TTL_SECONDS, SuggestionPolicy
from .trigger import (
    MESSAGE_RECEIVED_TYPE,
    OUTBOUND_MESSAGE_SENT_TYPE,
    OUTBOUND_REACTION_ADDED_TYPE,
    PERSONA_TRIGGER_TYPES,
    InboundMessage,
    Trigger,
    triggers_a_persona,
    type_of,
)

__all__ = [
    "Config",
    "ConfigError",
    "Context",
    "DEFAULT_SUGGESTION_TTL_SECONDS",
    "EnvelopeError",
    "FIRST_ATTEMPT",
    "GRANTED",
    "InboundMessage",
    "LANGUAGES",
    "LANGUAGE_NAMES",
    "Llm",
    "LlmAnsweredNothing",
    "LlmConfig",
    "LlmError",
    "LlmRefused",
    "LlmSpentItsBudgetThinking",
    "LlmUnreachable",
    "MESSAGE_RECEIVED_TYPE",
    "OUTBOUND_MESSAGE_SENT_TYPE",
    "OUTBOUND_REACTION_ADDED_TYPE",
    "PERSONA_TRIGGER_TYPES",
    "Persona",
    "SUGGEST_TYPE",
    "Suggestion",
    "SuggestionPolicy",
    "THINKING_TYPE",
    "Trigger",
    "USER_LANGUAGE_VARIABLE",
    "completion_text",
    "consent_of",
    "deterministic_id",
    "is_granted",
    "language_name",
    "nats_headers",
    "suggest_event",
    "suggest_id",
    "system",
    "thinking_event",
    "thinking_id",
    "triggers_a_persona",
    "type_of",
    "user",
]

#: The names that cost a third-party dependency, and the module each comes
#: from. Resolved on first attribute access (PEP 562).
_LAZY = {
    "Llm": "twalk_sdk.llm",
    "system": "twalk_sdk.llm",
    "user": "twalk_sdk.llm",
    "Context": "twalk_sdk.persona",
    "Persona": "twalk_sdk.persona",
}


def __getattr__(name: str) -> Any:
    module_name = _LAZY.get(name)
    if module_name is None:
        raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
    import importlib

    return getattr(importlib.import_module(module_name), name)


def __dir__() -> list:
    return sorted(__all__)
