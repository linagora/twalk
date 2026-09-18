"""assistant — the reference Twalk persona.

It reads the user's incoming messages and drafts a reply for them to
approve. It never sends: a suggestion becomes a reply only when a human
approval references it, which is the Hermes runtime's job (issue #23) and
the Sensor's outbound path after that.

Everything that must not be got wrong is the SDK's (`sdk/python/twalk_sdk`):
the consent gate runs before this file's handler is ever called, the
`thinking` event is published the moment processing starts, the returned
suggestion becomes a contract-valid `persona.suggest.produced` with the
contract's deterministic id, and the trigger's network, consent and trace
are carried through. What is left here is the persona's own judgement — the
prompt, and when to stay quiet — which is the point: a persona author
writes this much and no more.
"""

from __future__ import annotations

from typing import Optional

from twalk_sdk import Context, InboundMessage, Persona, Suggestion
from twalk_sdk.llm import system, user

#: Short, and deliberately about restraint: what the model returns is shown
#: to the user as a ready-to-send reply, so anything but the reply itself
#: (a preamble, an explanation, quotes around it) is noise the user has to
#: delete before approving.
SYSTEM_PROMPT = (
    "You draft replies to personal messages on behalf of the user. "
    "Answer with the reply text only: no preamble, no explanation, no quotes. "
    "Write in the same language as the message you are replying to. "
    "Keep it short and in the tone of a personal conversation. "
    "Never invent facts, commitments or times the message does not support; "
    "if the message needs information you do not have, draft a reply that asks for it."
)

#: A reply the user has to edit is worse than a short one, and a suggestion
#: is not an essay.
MAX_TOKENS = 300

#: Low, not zero: the same message should get the same draft on a retry.
TEMPERATURE = 0.2

persona = Persona.from_env()


@persona.on_inbound_message
async def draft_reply(message: InboundMessage, context: Context) -> Optional[Suggestion]:
    """Drafts one reply to one message the user consented to.

    Called only for events whose consent is `granted` — the SDK's gate has
    already dropped everything else, and no message content has been read
    at that point.
    """
    text = (message.body or "").strip()
    if not text:
        # v0.1 answers text. An attachment-only message (or one whose body
        # the Sensor could not read) is skipped quietly: no suggestion is a
        # valid outcome, and a persona that guesses at a photo would be
        # worse than one that says nothing.
        context.logger.info(
            "no text to reply to, skipping event_id=%s network=%s",
            message.event_id,
            message.network,
        )
        return None

    reply = await context.llm.complete(
        [system(SYSTEM_PROMPT), user(text)],
        temperature=TEMPERATURE,
        max_tokens=MAX_TOKENS,
    )
    return Suggestion(body=reply)


if __name__ == "__main__":
    persona.run()
