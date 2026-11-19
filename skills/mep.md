# Building services networks

## When to use
Read this before you draft ducts, pipes, cable trays or the equipment they serve: ventilation and
air conditioning, heating and cooling water, water supply, drainage, sprinklers, process piping on
racks, power and data in plant rooms. Do not use it for walls, floors, structure or furniture.

## Order of tools
1. Read first. `project_info`, `model_summary` and `list_levels`; `query_elements` for rooms,
   shafts, equipment and existing runs on the levels you will touch; `query_spatial` for what
   lies in the way. Know which systems already exist and what they are named before you add one.
2. Fix the systems before segments. For each system name its medium, its source (air handler,
   pump, main), the rooms or fixtures it serves and the riser or shaft it uses. Rooms with their
   own requirement (operating theatres, clean rooms, server rooms) get their own system, not a
   branch of a general one.
3. Propose in one group. `changeset_begin`, then place the equipment and fixtures, then
   `route_mep` from each source to its targets: state the intent (system, flow, friction or
   velocity, fall for drains) and let the router size and fit the network. Do not place segments
   point by point. Try a large batch with `ops_dry_run` first. Close with `changeset_commit`.
4. Check what you made. `run_checks` with `scope: {refs: [...]}` on the new runs and compare with
   a run before the writes: only new findings are yours. `clash` between the new runs and the
   structure. `query_topology` or `query_elements` to confirm every served room is connected and
   each system is one connected network with no open ends.
5. Look at it. `view_snapshot` of the plant room and one typical floor; say what you see.
6. `draft_status`, then `request_apply` with a title that names the systems and what they serve.

## Rules of thumb
- One system is one connected graph: segments meet at fittings or ports, never end in the air.
- A change extends the system that exists. Adding a room adds a branch; it does not start a
  second copy of the system.
- Sizes come from flow: if the reply reports a size or a pressure loss, quote it; do not round
  it into something it is not.
- Drains fall in the direction of flow; supply pipes do not need to.
- Keep the clear height the rooms ask for; route above ceilings and in shafts.
- When you state a number (flow, size, loss, count of served rooms), take it from a tool reply.

## Refusals and what to do
- `busy`: the editor did not take the call. Wait a moment and send the same call again.
- `route_mep` finds no path: read what blocks it (`query_spatial`, `clash`), choose another
  riser or level for the run, or report the obstacle to the person; do not force a run through
  structure.
- A write refused for an unknown system, type or connector: read the existing systems and types
  and use those, or create the type first, then retry once.
- A write refused for size: split by level or by system and commit each part on its own.
- A served room cannot be reached: say which room and why; never mark it served.
- The person discards the draft: start again from step 1, the model may have changed.
