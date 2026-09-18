"""The persona's one outside call: an OpenAI-compatible chat completion.

Deliberately the smallest client that does the job, and deliberately not
the vendor's SDK: "OpenAI-compatible" is a wire format, and a persona that
speaks it directly runs against llama.cpp, Ollama, vLLM, LiteLLM or a
remote provider with nothing but a different ``TWALK_LLM_BASE_URL``. The
operator chooses the endpoint, which is the only reason message content
ever leaves their infrastructure.
"""

from __future__ import annotations

from typing import Any, Dict, Mapping, Optional, Sequence

import httpx

from .config import LlmConfig


class LlmError(RuntimeError):
    """The endpoint refused the request, or answered something unusable."""


def system(content: str) -> Dict[str, str]:
    return {"role": "system", "content": content}


def user(content: str) -> Dict[str, str]:
    return {"role": "user", "content": content}


class Llm:
    """A chat-completions endpoint, as a persona uses it."""

    def __init__(self, config: LlmConfig, client: Optional[httpx.AsyncClient] = None) -> None:
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

        Raises :class:`LlmError` on anything else — a refused request, an
        answer with no choices, an empty completion. A persona that cannot
        reason produces no suggestion; it never invents one.
        """
        payload: Dict[str, Any] = {
            "model": self._config.model,
            "messages": [dict(message) for message in messages],
        }
        if temperature is not None:
            payload["temperature"] = temperature
        if max_tokens is not None:
            payload["max_tokens"] = max_tokens

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
            raise LlmError(
                f"the chat-completions endpoint at {self._config.chat_completions_url} "
                f"is unreachable: {error}"
            ) from error

        if response.status_code >= 400:
            # The body may carry the endpoint's own error message, which is
            # the operator's most useful clue; it is never message content.
            raise LlmError(
                f"the chat-completions endpoint answered HTTP "
                f"{response.status_code}: {response.text[:512]}"
            )
        try:
            body = response.json()
        except ValueError as error:
            raise LlmError("the chat-completions answer is not JSON") from error

        choices = body.get("choices")
        if not isinstance(choices, list) or not choices:
            raise LlmError("the chat-completions answer carries no choices")
        message = choices[0].get("message") if isinstance(choices[0], Mapping) else None
        content = message.get("content") if isinstance(message, Mapping) else None
        if not isinstance(content, str) or not content.strip():
            raise LlmError("the chat-completions answer carries no content")
        return content.strip()

    async def aclose(self) -> None:
        if self._owns_client:
            await self._client.aclose()
