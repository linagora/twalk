## Parent

#19 (Hermes — spec)

## What to build

Every reply a persona drafted carries a short disclosure to the person receiving it, stating that the message was written with the user's assistant and sent by the user. On by default; the user can turn it off, and turning it off is recorded as a deliberate act with a timestamp, not stored as a silent preference.

Decided with the repo's owner and recorded as ADR 0019. Raised by them during live testing, in these words: *"the agent does not announce itself and does not offer consent"* — after watching real conversations flow through the reference deployment.

Today the audit trail records who approved a reply (`approved_by` on `persona.reply.approved`), and that trail serves the operator: the one person who does not need telling. The contact receives a message indistinguishable from one the user typed.

## Acceptance criteria

- [ ] An approved reply that came from a persona carries the disclosure; a message the user wrote themselves carries nothing. The disclosure states a fact about how *this* message was produced, and attaching it to everything would make it meaningless.
- [ ] It is written in the language of the reply (ADR 0016), not the user's interface language, and exists in all five catalogues.
- [ ] It is on by default. The setting lives where the user can find it, and switching it off writes a dated record — the point is that a deployment can answer "since when, and who decided" later.
- [ ] The disclosure travels as part of the outbound message, so it reaches the contact on whatever network the conversation is on. It is not a Matrix-only decoration, and it is not metadata a bridge would drop.
- [ ] Tests: a reply approved from a suggestion reaches the network with the disclosure; the same text sent by the user directly does not carry it; with the setting off, the reply goes without it and the record of that choice exists.

## Notes

The two alternatives are argued in ADR 0019 and should not be relitigated in review without new information: disclosing once per conversation ("once" becomes "never", and a contact joining a group later never sees it), and disclosing nothing at all (coherent, and a position we would have to defend publicly one day).

The one judgement left to the implementer is the wording, which is the part that decides whether people accept it or switch it off. It must not read as a legal notice.

## Blocked by

None (can start immediately) — but it belongs with the approval path, so it is naturally sequenced with the full-loop ticket #25.
