# vex reference: change safety

Load this for implementation work, dependency questions, refactors, or review.

- Call `impact` with `direction="upstream"` before changing a shared symbol;
  upstream results are callers and dependents that may break.
- Use downstream impact only when you need to understand what the symbol uses.
- Treat confidence below `0.7` as a lead requiring receiver-type or source
  verification.
- After editing shared or multiple files, call `detect_changes`. Review changed
  symbols and high-confidence affected callers, then run tests proportional to
  that blast radius. `detect_changes(base='main')` also reports each changed
  symbol's shape (moved, body- or signature-edited, added, removed) and
  whether the change introduced each conditional.
- Direct reads and tests are verification. vex narrows the work; it does not
  replace evidence from the implementation or runtime.
