"""The persona's one outside call: an OpenAI-compatible chat completion.

Deliberately the smallest client that does the job, and deliberately not
the vendor's SDK: "OpenAI-compatible" is a wire format, and a persona that
speaks it directly runs against llama.cpp, Ollama, vLLM, LiteLLM or a
remote provider with nothing but a different ``TWALK_LLM_BASE_URL``. The
operator chooses the endpoint, which is the only reason message content
ever leaves their infrastructure.

What an answer *means* is deliberately not here: :mod:`twalk_sdk.completion`
reads it, so that a completion and the four ways there is no completion — an
unreachable endpoint, a refused request, a model that answered nothing, and a
model that spent its whole budget reasoning — are decided in a pure module and
tested without a network (issue #162). This file owns the transport and
nothing else.

The client answers two questions and both go through :meth:`Llm.complete`:
the reply itself, which the persona's handler asks for, and — after the
handler returns — **which language that reply is in**, which the SDK asks
for it (:meth:`Llm.language_of`, ADR 0031). One more completion per
suggestion, shaped for one token; the question is the model's to answer and
never a detection library's (ADR 0016), and reading the answer is
:mod:`twalk_sdk.disclosure`'s, pure.
"""

from __future__ import annotations

from typing import Dict, Mapping, Optional, Sequence

import httpx

from .completion import (
    LlmAnsweredNothing,
    LlmError,
    LlmRefused,
    LlmSpentItsBudgetThinking,
    LlmUnreachable,
    completion_text,
)
from .config import LlmConfig
from .disclosure import LANGUAGE_ASK, parse_language_answer

__all__ = [
    "Llm",
    "LlmAnsweredNothing",
    "LlmError",
    "LlmRefused",
    "LlmSpentItsBudgetThinking",
    "LlmUnreachable",
    "system",
    "user",
]


def system(content: str) -> Dict[str, str]:
    return {"role": "system", "content": content}


def user(content: str) -> Dict[str, str]:
    return {"role": "user", "content": content}


class Llm:
    """A chat-completions endpoint, as a persona uses it."""

    def __init__(
        self, config: LlmConfig, client: Optional[httpx.AsyncClient] = None
    ) -> None:
        self._config = config
        self._owns_client = client is None
        self._client = client or httpx.AsyncClient(timeout=config.timeout_seconds)

    @property
    def model(self) -> str:
        """The model name sent with every request, and reported in the
        ``thinking`` event so oversight can say what reasoned."""
        return self._config.model

    async def complete(
        self,
        messages: Sequence[Mapping[str, str]],
        *,
        temperature: Optional[float] = None,
        max_tokens: Optional[int] = None,
    ) -> str:
        """Sends one completion request and returns the assistant's text.

        Raises one of :mod:`twalk_sdk.completion`'s four named failures on
        anything else — an endpoint that could not be reached, one that
        refused the request, a model that answered nothing, and a model that
        spent its whole budget reasoning (issue #162). A persona that cannot
        reason produces no suggestion; it never invents one.

        The distinction is not cosmetic: the last of the four is the only
        one that cannot change on a second try, and it is metered, so the
        persona loop refuses the trigger instead of retrying it.
        """
        # The body, including the operator's provider parameters (ADR 0015),
        # is assembled by the configuration itself — see
        # `LlmConfig.chat_completions_payload`.
        payload = self._config.chat_completions_payload(
            messages, temperature=temperature, max_tokens=max_tokens
        )

        headers = {"content-type": "application/json"}
        if self._config.api_key:
            headers["authorization"] = f"Bearer {self._config.api_key}"

        try:
            response = await self._client.post(
                self._config.chat_completions_url,
                json=payload,
                headers=headers,
                timeout=self._config.timeout_seconds,
            )
        except httpx.HTTPError as error:
            raise LlmUnreachable(
                f"the chat-completions endpoint at {self._config.chat_completions_url} "
                f"is unreachable: {error}"
            ) from error

        if response.status_code >= 400:
            # The body may carry the endpoint's own error message, which is
            # the operator's most useful clue; it is never message content.
            raise LlmRefused(
                f"the chat-completions endpoint answered HTTP "
                f"{response.status_code}: {response.text[:512]}"
            )
        try:
            body = response.json()
        except ValueError as error:
            raise LlmRefused("the chat-completions answer is not JSON") from error

        # What the answer means is `twalk_sdk.completion`'s, tested without a
        # network. The budget it is told about is the one the request actually
        # carried — the operator's parameters are merged last, so it is not
        # necessarily the one the persona asked for.
        return completion_text(body, budget=_budget(payload))

    async def language_of(self, text: str) -> Optional[str]:
        """Asks the model which language ``text`` is written in.

        One completion — :data:`~twalk_sdk.disclosure.LANGUAGE_ASK` as the
        system prompt, the text as the user message, five tokens of budget
        and no temperature — and one token read back: the tag the model
        answered (``fr``, ``fr-ca``, ``ja``) or ``None`` for ``other``. What
        the tag selects, and what happens when it selects nothing, is the
        persona loop's (:mod:`twalk_sdk.persona`); this is only the ask.

        The same four failures as :meth:`complete`, because it is the same
        call: an endpoint that was briefly away is retried through the
        trigger's redelivery, like the reply's own completion.
        """
        answer = await self.complete(
            [system(LANGUAGE_ASK), user(text)], temperature=0, max_tokens=5
        )
        return parse_language_answer(answer)

    async def aclose(self) -> None:
        if self._owns_client:
            await self._client.aclose()


def _budget(payload: Mapping[str, object]) -> Optional[int]:
    """The ``max_tokens`` the request carries, if it carries one."""
    value = payload.get("max_tokens")
    return value if isinstance(value, int) and not isinstance(value, bool) else None
