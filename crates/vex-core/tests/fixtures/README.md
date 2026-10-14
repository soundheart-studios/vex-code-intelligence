# vex-core test fixtures

- `golden/`: five Parsed outputs copied byte-for-byte from the golden
  export at the revision in `golden/REVISION` (`parsed/<corpus>/<path>.json`,
  with `/` replaced by `_`). They were chosen to cover every field, a capped
  chunk (`full_chars`), residual chunks, a declaration site and escaped
  quotes and backslashes. No golden output contains a non-ASCII byte, so
  Unicode handling is covered by `oracle/` instead.
- `oracle/`: values produced by the reference implementation's serializer, chunk
  type and chunk-id format for inputs chosen to stress them: non-ASCII content, the 5,000 code-point cap, `setdefault`
  of `full_chars`, control characters, U+2028/U+2029, astral characters and
  key ordering.
