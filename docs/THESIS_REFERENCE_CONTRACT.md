# Thesis Reference Contract — MKO's side

This is MKO's half of the handoff contract with Thesis (a separate
repository). Thesis's own `docs/MKO_REFERENCE_CONTRACT.md` declares that it
changes only together with its MKO counterpart; this file is that
counterpart. Neither file makes a claim about the other repository's code —
each documents what its own side promises.

Authority: `docs/superpowers/specs/2026-08-28-frictionless-knowledge-design.md`
§4.4 (D9). That spec is the design record for this revision; resolve any
conflict in this file's favor of the spec, not the other way around.

## What MKO hands Thesis

MKO records enter Thesis as opaque `mko://` URI strings. Verified 2026-08-28:
Thesis reads no MKO approval state anywhere in its own code — the string is a
reference, not a payload, and Thesis does not parse or trust anything encoded
in it beyond identifying the record. The gate that actually blocks promotion
into Thesis evidence is Thesis's own human review status, set by a human in
the Thesis TTY. MKO does not, and cannot, promote anything into Thesis; MKO
has no Thesis-side write path.

## The eligibility rule

**Only records carrying the human-confirmation badge are eligible for
promotion.**

"The human-confirmation badge" is the Phase 0 replacement for what earlier
contract language called "MKO approval state" (see the Superseded field
below). A Source or Knowledge revision is complete the moment MKO's Core
writes it — there is no pending-until-approved lifecycle (spec §4.1). The
badge is a separate, derived fact: whether a human has confirmed that exact
revision in a real terminal (`mko confirm`), recorded as an `approve` event
in MKO's append-only review event graph and surfaced today as the derived
state `confirmed`.

This rule is **owner operating discipline today, not code**: the enforcing
gate remains Thesis's own human review status, and nothing in MKO or Thesis
mechanically checks the badge before a record is referenced by a Thesis
`mko://` URI. The owner is expected to promote only confirmed records by
their own judgment. Mechanical enforcement — Thesis actually reading MKO's
confirmation state before accepting a promotion — belongs to whenever Thesis
implements that handoff; this document does not authorize or describe that
implementation.

## Superseded field

Where an earlier revision of this handoff (or of Thesis's own contract
document) named an "MKO approval state" precondition, that field is replaced
by the human-confirmation badge described above. There was never a stored
"approval state" field on an MKO record to begin with — the prior language
described a precondition Thesis's contract declared, not implemented
behavior on the MKO side (spec §4.4). This document exists so that precondition
now names the real thing: a derived confirmation badge, not a blocking state.

## Out of scope

No Thesis code change is made or authorized by this document. It records
MKO's side of a documentation contract only.
