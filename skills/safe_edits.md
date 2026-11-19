# Safe edits of an existing model

## When to use
Read this before you change a model that already has content: add a floor, a platform or a room,
move or resize what exists, replace a type, delete, or fix a finding. It applies to every
discipline. Do not use it for a first build on an empty project; there the discipline guide
(structure or services) leads.

## Order of tools
1. Read the part you will change and its neighbours. `query_elements` with the narrowest filter
   that finds the targets (level, category, ids the person named); `get_element` for each target
   you will write to; `query_relations` for what is hosted on it or joined to it. Write down the
   ids: the edit touches these and nothing else.
2. Take a baseline. `run_checks` on the same scope before the edit, so findings after it can be
   told apart.
3. Propose the smallest change that does what was asked, in one group: `changeset_begin`, the
   writes (`update_elements` before delete-and-recreate, `replace_type` before editing every
   instance, `create_elements` only for what is new), `changeset_commit`. Use `ops_dry_run` for
   anything that touches more than a handful of elements.
4. Check the receipt. `draft_status` lists what is created, changed and deleted: every changed id
   must be one you wrote down in step 1 or one you created; nothing deleted unless deletion was
   asked. Then `run_checks` on the same scope as the baseline; new findings are yours to fix or
   report.
5. Look. `view_snapshot` of the changed area with its surroundings.
6. `request_apply` with a title that says what changed and where. The person applies it as one
   undo step.

## Rules of thumb
- Ask before you widen the scope. If the request is ambiguous about which elements, read and ask,
  do not guess across the whole model.
- Prefer editing parameters to rebuilding. A rebuild loses ids, hosted elements and history.
- Never delete to make a check pass.
- Keep the person's locks and rules. A refusal for a locked rule is an answer, not an obstacle.
- Say what you did not change when it matters ("the other floors keep their layout").

## Refusals and what to do
- `busy`: the editor did not take the call. Wait a moment and send the same call again.
- A write refused because an element is pinned or a rule is locked: tell the person which one
  and why; do not unpin or unlock on your own.
- A write refused because the target no longer exists: the model changed; read again from step 1.
- The receipt shows changes outside the targets: `discard_draft` and propose again with a
  narrower group; do not ask the person to apply a draft you cannot explain.
- The person discards the draft: start again from step 1.
