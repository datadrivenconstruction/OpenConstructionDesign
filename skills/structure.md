# Load-bearing structures

## When to use
Read this before you draft anything that carries load as a system of members: lattice towers and
masts, trusses, space frames, gridshells and diagrids, arches, bridges, stadium roofs, steel or
timber frames with connections, footings under them. Do not use it for finishes, colours, room
layouts or single walls and doors: those need no structural reasoning.

## Order of tools
1. Read first. `project_info` and `model_summary` for levels, units and what exists;
   `query_elements` for the members already in the area you will touch; `list_types` and
   `describe_category` for the sections and categories the model really has. Never assume a
   section, a category or a level that a read did not show.
2. Fix the scheme before members. State height, spans, number of legs or bays, taper, platform
   levels and the bracing pattern in one sentence each. A tower is legs + horizontals + diagonals +
   platforms + nodes; a frame is columns + beams + bracing; a shell is a surface + a grid of
   members + edge members + supports. If the brief gives a number, keep it; if it gives none, pick
   one, say which, and keep it consistent.
3. Propose in one group. `changeset_begin`, then the writes: `create_geometry` (gridshell,
   freeform, panelize) or `run_script` with a recipe from `script_recipes` for repeated members,
   `create_elements` for single columns, beams and slabs, `connect_members` for plates, bolts and
   welds at nodes. Try a large batch with `ops_dry_run` first: it says what would fail without
   touching the draft. Close with `changeset_commit`.
4. Check what you made. `run_checks` with `scope: {refs: [...]}` on the new members; compare with a
   `run_checks` you ran before the writes, and treat only findings that are new as yours.
   `query_topology` or `query_elements` to confirm counts (members, nodes, platforms) match the
   scheme you stated.
5. Look at it. `view_snapshot` (3D, the whole structure in frame) and say what you see: legs that
   do not meet, platforms at the wrong height, members floating free.
6. `draft_status`, then `request_apply` with a title that names the structure and its key numbers.

## Rules of thumb
- Members meet at nodes: a member end lies on another member or a support, never in the air.
- No two members on the same axis between the same nodes (duplicates double the count and the
  weight).
- Every load path ends in a support or a footing. A cantilever is stated, not accidental.
- Curved legs and arches are curves, not straight segments that only look curved from far away:
  check the deviation the reply reports.
- When you answer with a number (sway, mass, member count), take it from a tool reply, name the
  tool, and never invent it. If no tool measures it, say so.
- Bolts, plates or welds you mention must exist as connection details in the model.

## Refusals and what to do
- `busy`: the editor did not take the call. Wait a moment and send the same call again.
- A write refused for an unknown type, section or category: read `list_types` or
  `describe_category`, choose an existing one or create the type first, then retry once.
- A write refused for size: split the group by level or by part (legs, bracing, platforms) and
  commit each part on its own.
- The model has no category for what is asked (for example connection details or a shell
  surface): say plainly that the capability is missing. Do not imitate it with other categories.
- A check finding you cannot fix within the scheme: report it with its rule id; do not hide it.
- The person discards the draft: start again from step 1, the model may have changed.
