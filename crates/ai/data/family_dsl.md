# Family definition text (reference for family_draft)

A family is one text, one statement per line, `#` starts a comment. Lengths carry units (`mm`, `m`,
`in`), angles `deg`. Coordinates are from the insertion point: x right, y deep, z up. The text is data:
no paths, no includes. Check it with `family_validate`; offer it with `family_draft`. A refusal lists
findings by code and line; fix the lines named, keep the rest, send the full corrected text.

## Statements and parameters

Parametric components ("families") v0: named parameters (type/instance, formulas
with units, dependency ordering, cycles are errors), reference planes, bodies built from primitives
(extrude, revolve, sweep along a path, loft, cut), visibility by detail level and view,
a 2D plan symbol. The definition is text (one instruction per line, `#` starts a comment):

```text
family Table
category furniture            # furniture generic casework plumbing lighting equipment air_terminal electrical
host none                     # none wall floor ceiling face
ifc IfcFurniture TABLE        # optional: class and PredefinedType
param Width length type 1600mm
param LegH length type = Height - Top      # formula
param Material material type "Oak"
ref Left x = -Width / 2       # reference plane x = …; coordinates that reference it are "locked"
type "1600x800" Width=1600mm, Depth=800mm
extrude Top rect(Left, Front, Right, Back) z(Height - Top, Height) mat(Material) show(medium, fine, 3d)
revolve Bowl poly(0,0, 250mm,0, 250mm,180mm) axis(0, 0) angle(360deg)
sweep Rail circle(0, 0, 15mm) path(0,0,0, 1m,0,0, 1m,1m,0)
blend Leg rect(-30mm,-30mm,30mm,30mm) rect(-20mm,-20mm,20mm,20mm) z(0, LegH)
symbol rect(Left, Front, Right, Back) show(coarse, medium, fine, plan)
connector Waste pipe sanitary at(0, 100mm, 0) dir(0, 0, -1) size(50mm)   # network connection
mesh Body data(eJx...) scale(Width / 900mm, 1, 1) mat("Oak") show(medium, fine, 3d)  # mesh
feature Body data(eJx...)     # feature timeline; its parameters follow the family's
plant Crown part(foliage) form(Form) height(H) crown(W) trunk(D) clear(C) leaf(1) seed(Seed) mat(Leaves)
                              # procedural tree: part(wood | foliage), form round oval conical columnar
                              # weeping palm vase spreading (or its index), see `plant`
```

Coordinates are relative to the insertion point: x right, y into the depth, z up. `void` on a body subtracts its volume.

## Reference planes, constraints, nesting

Family editor: reference planes solved by a sketch, parameter-driven dimensions,
equalities, alignments, angle parameters, text conditions, per-parameter solid visibility,
insertion point and nested families. Extends the definition text:

```text
ref Center x = 0                 # pinned plane (as before)
ref Left x ~ -400mm              # free: the sketch solves its position, ~ is the initial value
refline Diag at(0, 0) angle(~30deg)   # reference line; the angle is a formula or ~free
dim Width Left Right             # Right − Left = Width (dimension tied to a parameter)
eq Left Center Right             # equal spacing
align Shelf Top                  # planes coincide (alignment lock)
angle Tilt Front Diag            # counter-clockwise angle from Front to Diag = Tilt
param Finish material type = if(Width > 1200mm, "Steel", "Oak")   # text condition
param Legs familytype type "Leg:Round"   # nested family type
visible Shelf HasShelf and Height > 600mm  # solid/nest is visible only when true
origin Left Front 0              # insertion point (expressions and planes allowed)
nest LegA Leg type(Round) at(Left, Front, 0) rot(0deg) set(Height = LegH, Mat = Finish)
nest LegB of(Legs) at(Right, Front, 0)     # family and type come from the familytype parameter
begin                            # nested definition up to `end` (after the family line)
family Leg
...
end
```

Parameter formulas cannot reference free planes and lines (the sketch solves them
after the parameters); solids, nests and the insertion point can.
