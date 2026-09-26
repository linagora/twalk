#!/usr/bin/env python3
"""The two governed reads, as a tool an agent can hold — and nothing else.

Why this exists rather than the two shell scripts beside it: on 2026-09-24
the reference deployment's drafting agent wrote a correct reply for the first
time, and the only way to let it run `freebusy.sh` had been to give its lane
a **terminal** — a shell on the owner's host, for an agent whose entire input
is text a stranger wrote. The first thing it did with that shell, unprompted,
was `cat` a `.env` and read `SOUL.md`. Those are the gestures of an agent
finding its way around; they are also, word for word, what a well-written
paragraph in an email could make it do to something else (#368).

So the capability is served instead of executed: this process speaks MCP over
stdin/stdout, offers exactly two functions, and can reach nothing but the
owner's own Companion Gateway. There is no third function, no file, no
process, no other host. An agent holding it keeps every freedom that matters
— whether to read, which windows, how many times — and loses the one nobody
meant to give it.

What crosses is what the Gateway already governs (ADR 0032): a free/busy read
capped at fourteen days, and what one event *carries* — never what it says.
Both are signed with the secret the outbound hook signs answers with, both are
recorded in the owner's journal, served or refused.

Stdlib only, one file, no build step: it installs the way the rest of this
skill does, by copying the directory.

    mcp_servers:
      twalk-calendar:
        command: python3
        args: ["/path/to/skills/twalk-calendar/mcp_server.py"]

with TWALK_GATEWAY_URL, TWALK_ANSWER_SECRET and TWALK_CALENDAR_CONNECTION in
the environment Hermes starts it with — the same three the scripts need.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import os
import sys
import time
import urllib.error
import urllib.request

#: The version we answer with when the client asks for one we do not know.
#: MCP's handshake echoes the client's version when the server supports it,
#: so a newer Hermes negotiates itself down rather than failing.
DEFAULT_PROTOCOL_VERSION = "2025-03-26"
KNOWN_PROTOCOL_VERSIONS = ("2024-11-05", "2025-03-26", "2025-06-18")

FREEBUSY_PATH = "/_twalk/hermes/freebusy"
EVENT_FACTS_PATH = "/_twalk/hermes/event-facts"

#: Longer than the Gateway's own relay to the collector, shorter than any
#: agent's patience: a read that has not answered by now will not.
TIMEOUT_SECONDS = 20.0


class Refused(Exception):
    """The Gateway said no, or the deployment never finished installing this.

    Carries the sentence the agent should act on — a refusal code where the
    Gateway gave one, and the name of the missing variable where it did not.
    """


def _setting(name: str, what: str) -> str:
    value = (os.environ.get(name) or "").strip()
    if not value:
        raise Refused(
            f"{name} is unset — {what}. This deployment has not finished "
            "installing twalk-calendar; nothing was read and nothing reached "
            "the Gateway, so this message is the only trace. Tell the owner's "
            "operator, by name, which variable is missing."
        )
    return value


def _encode(value: str) -> str:
    """Percent-encoding the Gateway's signature check agrees with.

    The query is signed **exactly as sent**, so this is not `urlencode`: it is
    the same small table the shell scripts use, kept in step with them
    deliberately. A `+` left bare arrives as a space and the read is refused
    as `invalid_window`, which sends the reader to look at their clock.
    """
    out = []
    for character in value:
        if character in "%:+ &=#?/":
            out.append(f"%{ord(character):02X}")
        else:
            out.append(character)
    return "".join(out)


def _read(path: str, query: str, reference: str | None) -> dict:
    """One signed GET, and the Gateway's answer or its refusal."""
    base = _setting("TWALK_GATEWAY_URL", "the owner's Companion Gateway origin")
    secret = _setting(
        "TWALK_ANSWER_SECRET", "the secret the outbound hook signs answers with"
    )
    timestamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    # The path is inside the signed line, so a signature made for one read is
    # refused on the other (#355). Four lines, in this order, no trailing
    # newline — the Gateway builds the same string and compares.
    signed = f"GET\n{path}\n{query}\n{timestamp}".encode("utf-8")
    signature = hmac.new(secret.encode("utf-8"), signed, hashlib.sha256).hexdigest()
    request = urllib.request.Request(
        f"{base.rstrip('/')}{path}?{query}",
        method="GET",
        headers={
            "X-Hermes-Timestamp": timestamp,
            "X-Hermes-Signature-256": f"sha256={signature}",
            # Every call is a line in the owner's record. A line that names the
            # message it was made for is one they can read back — *this draft
            # read my calendar, for that mail* — so the reference the agent was
            # given travels here when it passes one.
            "X-Hermes-Delivery": reference or f"mcp-{int(time.time() * 1000)}",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as answer:
            return json.loads(answer.read().decode("utf-8"))
    except urllib.error.HTTPError as refusal:
        body = refusal.read().decode("utf-8", "replace")
        try:
            document = json.loads(body)
        except ValueError:
            raise Refused(f"the Gateway refused the read with HTTP {refusal.code}: {body[:200]}")
        code = document.get("code") or document.get("error") or "refused"
        detail = document.get("message") or document.get("detail") or ""
        raise Refused(f"the Gateway refused the read: {code}. {detail}".strip())
    except urllib.error.URLError as unreachable:
        raise Refused(
            f"the owner's Companion Gateway did not answer ({unreachable.reason}). "
            "Say you could not check the calendar; do not guess a time."
        )


def freebusy(arguments: dict) -> dict:
    start = str(arguments.get("from") or "").strip()
    end = str(arguments.get("to") or "").strip()
    if not start or not end:
        raise Refused("from and to are both required, as RFC 3339 instants")
    connection = _setting(
        "TWALK_CALENDAR_CONNECTION", "the calendar connection this deployment reads"
    )
    query = (
        f"connection={_encode(connection)}"
        f"&from={_encode(start)}&to={_encode(end)}"
    )
    return _read(FREEBUSY_PATH, query, arguments.get("reference"))


def event_facts(arguments: dict) -> dict:
    uid = str(arguments.get("uid") or "").strip()
    if not uid:
        raise Refused(
            "uid is required: the event's iCalendar UID, exactly as "
            "calendar.event.* carried it in data.uid. This read does not "
            "search by title or by time."
        )
    connection = _setting(
        "TWALK_CALENDAR_CONNECTION", "the calendar connection this deployment reads"
    )
    query = f"connection={_encode(connection)}&uid={_encode(uid)}"
    return _read(EVENT_FACTS_PATH, query, arguments.get("reference"))


#: The two functions, and the descriptions the model decides from. They say
#: when to reach for the read and what the answer is *not*, because a tool
#: whose description stops at "reads the calendar" invites the two questions
#: this deployment refuses: what is the owner doing, and with whom.
TOOLS = [
    {
        "name": "freebusy",
        "description": (
            "When the owner is busy, inside a window of at most fourteen days. "
            "Read it BEFORE writing any reply that proposes a time, accepts one, "
            "or agrees to move a meeting — including when the contact proposed "
            "the times themselves, because picking one looks like agreeing and "
            "is in fact scheduling. The answer is a list of busy intervals in "
            "UTC, end exclusive; every gap between them is free. It never says "
            "what the owner is doing or with whom, and you must not guess: "
            "\"Thursday morning is taken\" is right, \"Thursday morning she has "
            "a meeting\" is not. Call it again, with another window, when the "
            "first comes back full. "
            "The answer also carries the owner's own time: `timezone` (an IANA "
            "name), `timezone_source` (where that came from), and `now` (what "
            "time it is there, with its offset). Convert every hour you write "
            "into that zone and name it — \"jeudi 12h30 (heure de Paris)\" — and "
            "count `next week` and `tomorrow` from `now`, not from your own "
            "idea of today. When those three are absent the deployment knows "
            "no zone: write your times in UTC and say they are UTC, rather "
            "than guessing a zone, because an hour in the wrong zone reads "
            "perfectly and is wrong."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "from": {
                    "type": "string",
                    "description": "Start of the window, RFC 3339 (2026-09-28T06:00:00Z).",
                },
                "to": {
                    "type": "string",
                    "description": (
                        "End of the window, RFC 3339, after `from` and at most "
                        "fourteen days later. Ask for the days the conversation "
                        "is about, not for the whole fortnight."
                    ),
                },
                "reference": {
                    "type": "string",
                    "description": (
                        "The TWALK-REF token you were given, so that this read "
                        "is recorded against the message it was made for."
                    ),
                },
            },
            "required": ["from", "to"],
        },
    },
    {
        "name": "event_facts",
        "description": (
            "What one of the owner's events carries: whether it has a join link "
            "and which, how long its description is, how many attachments. "
            "Named by the uid the event that woke you carried, never by title or "
            "time. It never returns the description's text — that is a free-text "
            "box written by whoever created the meeting, and no route answers it. "
            "\"found\": false means this deployment holds no event with that uid, "
            "not that the meeting carries nothing: say you cannot tell."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "uid": {
                    "type": "string",
                    "description": "The event's iCalendar UID, as calendar.event.* carried it in data.uid.",
                },
                "reference": {
                    "type": "string",
                    "description": "The TWALK-REF token you were given, for the owner's record.",
                },
            },
            "required": ["uid"],
        },
    },
]

HANDLERS = {"freebusy": freebusy, "event_facts": event_facts}


def _result(payload: dict) -> dict:
    """An answer the model reads as text, because that is what MCP carries.

    `structuredContent` beside it for a client that would rather parse than
    read; the text is the part every client has.
    """
    return {
        "content": [{"type": "text", "text": json.dumps(payload, ensure_ascii=False)}],
        "structuredContent": payload,
    }


def _refusal(said: str) -> dict:
    """A refused read is a result, not a protocol error.

    The difference matters: a JSON-RPC error tells the agent the *tool* is
    broken, which it is not, and some clients then stop offering it. A result
    marked `isError` puts the sentence in front of the model, which is the
    one that has to decide what to say to a human about it.
    """
    return {"content": [{"type": "text", "text": said}], "isError": True}


def handle(request: dict) -> dict | None:
    """One request in, one response out — or None for a notification."""
    method = request.get("method")
    request_id = request.get("id")

    if method == "initialize":
        asked = (request.get("params") or {}).get("protocolVersion")
        version = asked if asked in KNOWN_PROTOCOL_VERSIONS else DEFAULT_PROTOCOL_VERSION
        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "twalk-calendar", "version": "1"},
            },
        }

    if method in ("notifications/initialized", "initialized"):
        return None

    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": request_id, "result": {"tools": TOOLS}}

    if method == "tools/call":
        params = request.get("params") or {}
        name = params.get("name")
        handler = HANDLERS.get(name)
        if handler is None:
            return {
                "jsonrpc": "2.0",
                "id": request_id,
                "result": _refusal(
                    f"this server offers {', '.join(HANDLERS)} and nothing else; "
                    f"{name!r} is not one of them"
                ),
            }
        try:
            payload = handler(params.get("arguments") or {})
        except Refused as refusal:
            return {"jsonrpc": "2.0", "id": request_id, "result": _refusal(str(refusal))}
        except Exception as unexpected:  # noqa: BLE001 — never take the server down
            return {
                "jsonrpc": "2.0",
                "id": request_id,
                "result": _refusal(f"the read could not be made: {unexpected}"),
            }
        return {"jsonrpc": "2.0", "id": request_id, "result": _result(payload)}

    if request_id is None:
        return None
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "error": {"code": -32601, "message": f"method not found: {method}"},
    }


def main() -> int:
    # Said once, at startup, on stderr: an operator reading the gateway's log
    # sees the missing variable there rather than in an agent's reply hours
    # later. Not fatal — the server still answers, and says the same thing to
    # the agent that calls it, which is who has to tell the human.
    for name in ("TWALK_GATEWAY_URL", "TWALK_ANSWER_SECRET", "TWALK_CALENDAR_CONNECTION"):
        if not (os.environ.get(name) or "").strip():
            print(f"twalk-calendar: {name} is unset; every read will be refused", file=sys.stderr)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except ValueError:
            continue
        response = handle(request)
        if response is not None:
            sys.stdout.write(json.dumps(response, ensure_ascii=False) + "\n")
            sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
