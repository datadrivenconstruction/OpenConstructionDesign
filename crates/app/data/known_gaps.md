# Known gaps

Checked by `crates/app/src/known_gaps.rs`: the stub table must equal the registry commands that
have neither a handler nor a branch in `EditorApp::run_cmd`, and that set must be empty. Every gap
row has a size: S (under a day), M (days), L (a week or more).

## Command stubs

| Command | Note |
|---|---|

## Gaps

| Gap | Size | Area | Next step |
|---|---|---|---|
| Divide beams, pipes and ducts into parts | M | modify | `divide_member(id, n)` in the model on the member axis, then route `mod.divide` by selection kind |
| Split a floor by a drawn line | L | modify | polygon cut in geom, two slabs with openings kept, one undo step |
| Arc walls: divide preview draws chords | S | modify | draw ticks at arc points from `ArcAxis::point_at` |
| Selection dimensions: grids and reference planes as neighbours | S | annotate | add grid axes to `sel_dims::for_wall`, lock as `LockDistGrid` |
| Background build of plan and section views | M | views | reuse `bg_latest::Latest` with `Model::worker_copy` as the sheet canvas does |
| Sheet canvas copy cost on very large models | M | views | keep one worker copy and replay the change journal instead of a full backup per revision |
| Saved workspaces: menu, rename dialog and quick-switch keys | S | shell | wire `ddc_ui::saved_ws` into the layout menu next to the built-in layouts |
