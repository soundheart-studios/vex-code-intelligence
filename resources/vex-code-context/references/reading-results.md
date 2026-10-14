# vex reference: reading a response

Load this when a response carries a field you have not acted on before, or
when deciding whether an answer is good enough to stop on.

## Scores say rank, not confidence

`score` is relative to the best hit IN THAT RESPONSE, so the top result is
always 1.0 whether or not it is any good. The information is in the SPREAD,
which the `ranking` block reports:

- `top_gap`: distance between the first and second hit. A large gap means a
  decisive winner. A flat slate (1.0, 0.98, 0.97) means the ranking is
  ambiguous, not that nothing is relevant.
- `verdict`: present only when that gap is too small to call a winner.
- `exact_symbol`: either the query named an identifier nothing answers to, in
  which case these are approximate matches and the top hit is not the
  definition of a symbol that does not exist; or the name is PART of real
  symbols, which it then lists. Query those names rather than the fragment.
- `top_gap: null`: only one result, so decisiveness is unmeasured rather than
  perfect.

## Say what matched

`matched_by` distinguishes the kinds of evidence: `definition` (this chunk
DEFINES the name you asked for) and `full_text` (lexical). A `definition` hit
is the strongest answer to "where is X".

## Partial source announces itself

- `truncated: true` with `recovery`: the body was cut when the chunk was
  written. Follow `recovery.next[0]`; it re-reads the span from the file.
- `part_of`: the hit is the part of a larger chunk that matched. You already
  have the matching span. It carries `tokens_here` and `tokens_whole`, so
  expand only when this span genuinely is not enough.

A response that carries neither is complete. Do not open the file to check.

## A context that held edges back says which kind

`context` returns at most 25 edges per direction, strongest first, and a
`context_edges_withheld` pointer separates two different absences:

- `below_confidence_floor`: name matches with no receiver type behind them.
  Get them with `context(..., confidence_min=0.0)`, and verify before
  trusting: a generic member name scores the same whatever it belongs to.
- `over_edge_cap`: real edges that did not fit. Walk them with
  `impact(symbol, direction='both', depth=1)` rather than re-asking.

## Working to a budget

`search(..., max_bytes=N)` caps a single query's response. Results are dropped
from the end, the best kept in order, and `ranking.budget` reports how many
were withheld and the call that returns them. The default is no cap.

Prefer a small `top_k` and a second call over one large request: a miss costs
one small response, and the pool behind the first result is deep, so asking
narrow first is cheap rather than risky.
