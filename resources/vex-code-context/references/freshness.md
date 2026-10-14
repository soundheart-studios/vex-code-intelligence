# vex reference: freshness

Load this when results appear stale, files changed, or a vex call fails.

## When a call fails

- A reply beginning `VEX ERROR (<code>)` is a failed call. The message carries
  the fix; quote it to the user rather than working around it.
- `no_index`: the project is not indexed yet. Run `vex init` in the
  repository (or pass `project=` naming one that is).
- `not_found` on a `project` argument: call `list_projects` and pass one of
  the ids, names or roots it lists.
- A repository whose index is missing or broken answers nothing, which looks
  exactly like a repository where the thing you asked about is absent. Do not
  report absence on the strength of a failing index, and do not quietly
  switch to another tool.

## Stale answers

- When the index is behind the working tree, responses carry a `freshness`
  block (`index_stale`, `index_age_s`, `reindex`). The answer is still served
  from the index as it was.
- Right after editing files, pass `force_fresh=true` to `search`: vex brings
  the index up to date first, then answers.
- `reindex` is incremental and content-hash authoritative: only changed files
  are re-processed, and calling it when nothing changed is a safe no-op.
  `reindex(force=true)` rebuilds everything.
- If a vex result conflicts with the current files, stop using that result
  and reindex immediately.
