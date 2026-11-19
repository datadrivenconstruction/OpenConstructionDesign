# Steel section tables

These rolled-section tables hold **nominal dimensions only**. Every section property is
computed by OpenConstructionDesign in `crates/geom/src/steel/rolled.rs`. The tables contain
no property columns taken from any published table.

| File | Series | Dimensions per | Unit |
|---|---|---|---|
| `en_ipe_he_upn.csv` | IPE, HEA, HEB, HEM, UPN | EN 10365 | mm |
| `uk_ub_uc.csv` | UB, UC | BS 4-1 | mm |
| `ru_gost57837.csv` | parallel-flange I-beams, series B, Sh, K | GOST 57837 | mm |
| `us_w.csv` | W shapes | ASTM A6 | in |

Provenance line for every row: "dimensions per <standard>; properties computed by
OpenConstructionDesign".

## Columns

`name,alias,kind,h,b,tw,tf,r1,r2,slope`

- `name`: the designation as the standard writes it. `alias` is a Latin spelling, used where
  the designation has non-Latin letters (GOST: Б = B, Ш = Sh, К = K).
- `kind`: `I` has parallel flanges. `U` is a channel with tapered flanges; its `tf` is
  measured at b/2 from the back of the web.
- `h, b, tw, tf`: depth, flange width, web and flange thickness.
- `r1`: root radius. `r2`: toe radius (channels). `slope`: inner flange slope (channels:
  0.08 up to UPN 300, 0.05 above).
- W shapes: `r1 = k - tf`, using the design k distance.

## Clean-room method

- Inputs are only the nominal dimensions of standard profiles. These are facts that national
  standards and many manufacturer catalogues publish. They were entered as numbers. No other
  section-table file, data repository or property column was opened, copied or used.
- Properties are computed when the table is first loaded, not stored. The data therefore
  stays dimensions only, and any change to a formula applies to every row without
  regenerating anything. All rows load in well under a millisecond.
- Formulas (see the module docs):
  - parallel-flange I: exact closed form of the plates plus four quarter-circle fillets;
  - channels: exact polygon integrals over the filleted outline;
  - It: the fillet junction formula of El Darwish and Johnston (1965) for I sections, thin
    plates for channels;
  - Iw: Iz (h - tf)^2 / 4 for I sections, and the thin-walled closed form for channels.
- Checks in `steel/tests_rolled.rs`:
  - closed forms worked by hand (rectangle, sharp I, sharp channel);
  - the closed form against the polygon route;
  - a few well-known published values, as tolerances;
  - the designation mass (UB, UC, W) of every row, within 2.5 %.

`sections.csv` (own compilation of common metric shapes) and `sections_inch.csv` (inch
channel, angle and hollow examples) stay as they are. Where a rolled designation already
appears there, `steel::sections::find` returns the same shape from either table.

Open check: the GOST 57837 rows and the UPN inner flange slope values are still to be
compared against the published standards before they are relied on for design.
