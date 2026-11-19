# edit3d — geometry editing in the 3D view

One registry per concern; other modules plug in without touching shared files.

**Scene bodies** — `scene3d::SceneProvider` (category → meshes + pick targets).
- The base is `mep3d::scene` (walls, slabs, …, MEP runs and components, instanced); built-in
  providers add roofs, ceilings, columns, beams, stairs, railings, imported meshes (`mesh`
  table) and pick-only rooms.
- Scene element ids: all ranges live in `crate::scene_ids` (compile-time non-overlap asserts);
  a new range goes there, never a local constant.
- `scene3d::compose_with(ed, &mut e3.cache)` rebuilds only parts of changed tables/levels
  (change journal) and keeps unchanged `Arc<SceneMesh>`; active-level parts get
  `EXT_BASE + k` mapped to `targets[k]`, other levels `OTHER_LEVEL`.
  `Viewport.e3.set_ext(targets)` hands the map to picking (done in `prepare_3d` and `new`).
- `SceneProvider::tables()` lists the model tables the provider reads (dirty tracking).
- New category (footings, trusses, sweeps, terrain, solids, masses…):
  1. pick a target: `pick::Cat::Ext(n)` with a unique `n` (or a new `Cat` variant);
  2. `impl SceneProvider { key, parts(ed, level) -> Vec<Part>, pick_only?, rank? }` —
     positions in the scene frame (x, y model metres; z minus `ed.elevation`);
  3. `scene3d::register(Arc::new(MyProvider))` once at start-up (e.g. from the module's
     `register(a)` command hook). The same `key` replaces a built-in provider.
- Use `Part::push(mesh, ed.elevation)` for model meshes with absolute z.

**Grips** — `grips::Provider` (`owns`, `grips`, `apply`, optional `preview`, `bodies`),
`grips::register(Arc<dyn Provider>)` (idempotent by `Provider::key`); extras are asked
before MEP runs (`mep_grips`) and the built-in one. Deltas are applied in f64 to the model
values; errors are `Error::Invalid("e3d.err.<key>")`, translated in the status line.
`apply` must be one model op: `edit3d` wraps it in `Editor::run_op` (one undo step).

**Transforms** — `xform::apply` moves/rotates/copies targets via existing model ops;
categories without `Target::modify` need a branch there (or a grips provider).

**Mirror, Array** — `mirror_array`: the plan commands (`mod.mirror`, `mod.array`) start the 3D tools
when the 3D view is active. Families come from `elem::Family.xf` (reason keys on refusal) and the
plan target map; the model does the work in one op (`mirror_elements`, `array_linear`,
`array_radial`). Ghost = `Xform::point` over the selected bodies.

Labels: `e3d.cat.<Cat::key()>` (`Ext` → `other`) in `ui/data/i18n/{ru,en}/<fragment>.json`.
