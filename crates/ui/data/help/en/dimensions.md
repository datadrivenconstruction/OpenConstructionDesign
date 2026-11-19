# Dimensions
@tags dimension, spot elevation, temporary dimension, measure
@context ann.dim ann.dim_angular ann.dim_radial ann.spot_elev ann.dim_auto mod.measure
@commands ann.dim ann.dim_angular ann.dim_radial ann.spot_elev ann.dim_auto mod.measure
@see tags_text units snaps

Dimensions are permanent (a view annotation) or temporary (shown while placing or selecting an element); a number in a temporary dimension can be edited.

**Not available yet:** spot elevation (`SE`) and measuring (`MS`). The ribbon buttons of these commands are inactive.

## steps
1. Linear dimension: `DI` ([[cmd:ann.dim]]) - click references one after another (`Tab` switches axis / face), then click empty space to place the dimension line.
2. Automatic chain along a wall: `AD` ([[cmd:ann.dim_auto]]) - pick a wall to get openings and piers. One wall extends to the collinear walls it touches; several selected walls give one chain per touching collinear run. Arc walls are skipped, and the status bar shows how many.
3. Angle between two walls - `DA` ([[cmd:ann.dim_angular]]), radius of an arc wall - `DR` ([[cmd:ann.dim_radial]]).
4. Spot elevation - `SE` ([[cmd:ann.spot_elev]]).
5. Measuring a distance without creating anything - `MS` ([[cmd:mod.measure]]).
6. To change a value in a temporary dimension click the number and type a new one.

## keys
- `DI` `DA` `DR` - linear, angular, radial dimension
- `AD` - wall chain, `SE` - spot elevation
- `MS` - measure
- `Tab` - switch axis / face while picking a reference

## tips
- Units and precision: [[topic:units]]; you can type `3500`, `3.5m`, `12'6"`.
- Dimensions pick up grids and wall faces: use `Tab` to switch between them.
- Temporary dimensions show distances to neighbouring walls and grids while you place an element.
