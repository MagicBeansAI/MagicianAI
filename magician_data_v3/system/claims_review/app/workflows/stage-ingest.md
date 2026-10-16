Record one structured transcript-ingest request in this package.

Before projecting anything, require `speaker_mapping_json` to be a closed JSON
object with exactly `speakers` and `utterances`: `speakers` maps explicit
speaker keys to non-empty named-person strings; each ordered utterance contains
exactly a `speaker` key present in that mapping and non-empty `text`. Reject
unknown keys, unmapped utterances, duplicate speaker keys, more than 64
speakers, more than 1,000 utterances, or transcript text over 131,072 bytes.
Never guess attribution from raw prose.

Project one `ingest_request`, copying every input verbatim, setting
`actor_ref` and `act_ref` to null, `apply_state` to `recorded`, and
`submitted_at` to current UTC. This workflow does not call the host ingest and
does not extract claims. A trusted host may stamp `actor_ref` from the
authenticated actor when it later applies the request; the frame supplies no
actor authority.

Only the canonical host `TranscriptIngestion` entry may apply this request. It
is act-always: an accepted request must durably create its act row even when
extraction understands nothing and yields zero claims. Claim count is never an
apply receipt. Only the host act reference may later set `act_ref` and the
applied projection state.
