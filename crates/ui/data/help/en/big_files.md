# Heavy models
@tags large files, performance, slow, LOD, level of detail
@commands view.section_box mod.isolate view.hide_cat view.detail_level mgr.purge
@see section_box visibility views_3d import_rvt

Large models load in the background and stay responsive if you limit what is drawn. The main tools are isolation, a section box, hiding categories and level of detail.

**Not available yet:** hiding a category in plan, the detail level and "Purge unused". The ribbon buttons of these commands are inactive.

## steps
1. A `.rvt` or `.ifc` file is loaded from the "Import" window ([[topic:import_rvt]], [[topic:import_ifc]]); a `.rvt` load runs in the background with a progress bar and a Cancel button in that window.
2. In 3D limit the scene with a section box around a storey or zone: `BX` ([[cmd:view.section_box]]).
3. Isolate what you need - `HI` ([[cmd:mod.isolate]]); hiding a whole category - [[cmd:view.hide_cat]].
4. A lower view detail level ([[cmd:view.detail_level]]) is planned.
5. Removing unused types and families: [[cmd:mgr.purge]].

6. A large model file can be opened in the background: [[cmd:mgr.open_big]]; the mesh cache ([[cmd:mgr.mesh_cache]]) speeds up 3D for imported meshes. In 3D a category can be hidden temporarily: [[cmd:view.hide_cat_3d]].

## keys
- `BX` - section box, `HI` - isolate, `HC` - hide category
- `HR` - reveal all

## tips
- Work in the plan of the level you need and use 3D only to check.
- Import a large `.rvt` or `.ifc` in parts: needed levels first ([[topic:import_rvt]]).
- With a section box the scene is drawn only inside the box: the smaller the box, the faster the frame.
