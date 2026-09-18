"""A persona's configuration, read from the environment.

Everything a persona needs is an environment variable, so a persona joins
the compose deployment the way the Sensor does. The values are validated
here, at startup, against the same patterns the contract's schemas use for
the fields they end up in: a persona id that could not appear in a valid
``source`` fails loudly on the first line instead of producing events no
consumer will accept.
"""

from __future__ import annotations

import os
import re
from dataclasses import dataclass
from typing import Mapping, Optional

#: ``data.persona_id`` and the last segment of ``source`` in every
#: ``persona.*`` schema.
PERSONA_ID_PATTERN = re.compile(r"^[a-z0-9_-]+$")

#: The bus namespace every Twalk deployment publishes under: the contract's
#: event types map to subjects by replacing the ``fr.linagora.twalk.``
#: prefix with it (see :meth:`Config.subject`).
DEFAULT_SUBJECT_PREFIX = "twalk"
DEFAULT_STREAM = "twalk"
DEFAULT_NATS_URL = "nats://localhost:4222"

#: The model name reported for oversight when the operator names none. The
#: endpoint is what decides which model answers; a local llama.cpp or
#: Ollama ignores the field entirely.
DEFAULT_MODEL = "local-model"

CONTRACT_TYPE_PREFIX = "fr.linagora.twalk."


class ConfigError(RuntimeError):
    """The environment does not describe a runnable persona."""


@dataclass(frozen=True)
class LlmConfig:
    """Where the persona reasons, and with what credentials.

    Any OpenAI-compatible chat-completions endpoint will do — that is the
    point: the operator develops against a model on their own machine and
    deploys against whichever endpoint they chose, and no message content
    reaches an endpoint they did not configure.
    """

    base_url: str
    model: str = DEFAULT_MODEL
    api_key: Optional[str] = None
    timeout_seconds: float = 60.0

    @property
    def chat_completions_url(self) -> str:
        return f"{self.base_url.rstrip('/')}/chat/completions"


@dataclass(frozen=True)
class Config:
    """One persona process's whole configuration."""

    persona_id: str
    hermes_domain: str
    llm: LlmConfig
    nats_url: str = DEFAULT_NATS_URL
    stream: str = DEFAULT_STREAM
    subject_prefix: str = DEFAULT_SUBJECT_PREFIX
    consumer_name: Optional[str] = None
    log_level: str = "info"

    def __post_init__(self) -> None:
        if not PERSONA_ID_PATTERN.match(self.persona_id):
            raise ConfigError(
                "TWALK_PERSONA_ID must match "
                f"{PERSONA_ID_PATTERN.pattern} (the contract's persona_id "
                f"pattern), got {self.persona_id!r}"
            )
        if not self.hermes_domain or "/" in self.hermes_domain:
            raise ConfigError(
                "TWALK_HERMES_DOMAIN must be a domain without a slash (it "
                f"is the authority of the event's source URI), got "
                f"{self.hermes_domain!r}"
            )
        if not self.llm.base_url:
            raise ConfigError("TWALK_LLM_BASE_URL must name a chat-completions endpoint")

    @property
    def source(self) -> str:
        """The ``source`` of every event this persona publishes."""
        return f"hermes://{self.hermes_domain}/personas/{self.persona_id}"

    @property
    def durable_name(self) -> str:
        """The durable consumer's name on the bus.

        Durable, so a persona restart resumes where it stopped instead of
        losing events — and named after the persona, so the same persona
        always comes back to the same consumer.
        """
        return self.consumer_name or f"persona-{self.persona_id}"

    def subject(self, event_type: str) -> str:
        """The bus subject of a contract event type.

        ``fr.linagora.twalk.<domain>.<action>.<version>`` becomes
        ``<prefix>.<domain>.<action>.<version>``, exactly as the Sensor maps
        it (``sensor/src/normalize.rs``).
        """
        if not event_type.startswith(CONTRACT_TYPE_PREFIX):
            raise ValueError(
                f"{event_type!r} is not a contract event type (no "
                f"{CONTRACT_TYPE_PREFIX} prefix)"
            )
        return f"{self.subject_prefix}.{event_type[len(CONTRACT_TYPE_PREFIX):]}"

    @classmethod
    def from_env(cls, env: Optional[Mapping[str, str]] = None) -> "Config":
        """Reads the configuration from ``os.environ`` (or a given mapping)."""
        env = os.environ if env is None else env

        def required(name: str) -> str:
            value = (env.get(name) or "").strip()
            if not value:
                raise ConfigError(f"{name} is required")
            return value

        def optional(name: str, default: str) -> str:
            value = (env.get(name) or "").strip()
            return value or default

        timeout_raw = optional("TWALK_LLM_TIMEOUT_SECONDS", "60")
        try:
            timeout_seconds = float(timeout_raw)
        except ValueError as error:
            raise ConfigError(
                f"TWALK_LLM_TIMEOUT_SECONDS must be a number of seconds, got {timeout_raw!r}"
            ) from error

        api_key = (env.get("TWALK_LLM_API_KEY") or "").strip() or None
        return cls(
            persona_id=required("TWALK_PERSONA_ID"),
            hermes_domain=required("TWALK_HERMES_DOMAIN"),
            llm=LlmConfig(
                base_url=required("TWALK_LLM_BASE_URL").rstrip("/"),
                model=optional("TWALK_LLM_MODEL", DEFAULT_MODEL),
                api_key=api_key,
                timeout_seconds=timeout_seconds,
            ),
            nats_url=optional("TWALK_NATS_URL", DEFAULT_NATS_URL),
            stream=optional("TWALK_BUS_STREAM", DEFAULT_STREAM),
            subject_prefix=optional("TWALK_BUS_SUBJECT_PREFIX", DEFAULT_SUBJECT_PREFIX),
            consumer_name=(env.get("TWALK_PERSONA_CONSUMER") or "").strip() or None,
            log_level=optional("TWALK_LOG_LEVEL", "info"),
        )
