# Showcase projects

Reproducible models built in code through the public model API: `office`, `residential` and
the `edge_*` set (`mod.rs` lists them in `PROJECTS`). Every test that runs over them runs over
all of them:

- `crates/model/tests/showcase_scenarios.rs`: model check, same build twice, save and reopen
  with identical content and bytes, undo of everything back to a fresh model;
- `crates/io/tests/showcase_xlsx.rs`: an untouched book (metric, imperial) is no change, one
  number edit per sheet kind applies, re-exports clean and undoes;
- `crates/app/tests/e2e_showcase.rs`: loading into the editor, the family book, an XLSX edit
  undone in one step.

The io and app tests include this folder with
`#[path = "../../model/tests/showcase/mod.rs"] mod showcase;`; it needs only `ddc_model` and
`rusqlite`.

## Adding a project

1. Write `pub fn my_case(m: &mut Model)` in a file here (or in `edge.rs`). Use the helpers in
   `mod.rs` (`wall_type`, `wall`, `ring`, `door`, `window`, `rect`, `lib_type`, ...) and
   `.unwrap()` every call, so a refused call names its line.
2. Add a `Project { name, about, build }` entry to `PROJECTS`. The name is stable: tests and
   docs refer to it.
3. Keep it deterministic: no clocks, no randomness, no `set_current_phase` (it writes outside
   the undo journal). Non-ASCII text goes in as `\u{..}` escapes.
4. Keep it small enough for debug builds. A large variant goes behind an
   `#[ignore = "..., slow in debug builds"]` test (see `huge_level_n`).
5. `validate` must find nothing. A rule that every element of a kind must follow belongs in
   `KIND_RULES`.

`build` adds a material row for every material text a project uses. `build_raw` leaves them
out; opening a current file never adds them (only an upgrade of an older file does).
