"""twalk_sdk — write a Twalk persona without touching NATS or CloudEvents.

A persona is a process that consumes inbound events from the bus and
publishes thinking and suggestion events back onto it. This SDK is the part
of that which is the same for every persona:

* the **consent gate** (:mod:`twalk_sdk.consent`), applied by the SDK
  before any persona code runs, so that an author cannot breach consent by
  forgetting;
* the contract's envelopes and their deterministic ids
  (:mod:`twalk_sdk.envelope`);
* the durable subscription, the publishing and the process loop
  (:mod:`twalk_sdk.persona`);
* an OpenAI-compatible chat-completions client (:mod:`twalk_sdk.llm`).

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
from .trigger import MESSAGE_RECEIVED_TYPE, InboundMessage, Trigger

__all__ = [
    "Config",
    "ConfigError",
    "Context",
    "EnvelopeError",
    "FIRST_ATTEMPT",
    "GRANTED",
    "InboundMessage",
    "Llm",
    "LlmConfig",
    "LlmError",
    "MESSAGE_RECEIVED_TYPE",
    "Persona",
    "SUGGEST_TYPE",
    "Suggestion",
    "THINKING_TYPE",
    "Trigger",
    "consent_of",
    "deterministic_id",
    "is_granted",
    "nats_headers",
    "suggest_event",
    "suggest_id",
    "system",
    "thinking_event",
    "thinking_id",
    "user",
]

#: The names that cost a third-party dependency, and the module each comes
#: from. Resolved on first attribute access (PEP 562).
_LAZY = {
    "Llm": "twalk_sdk.llm",
    "LlmError": "twalk_sdk.llm",
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
