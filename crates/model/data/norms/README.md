# Norm packs

Schema: `pack.schema.json`; checker: `crates/model/src/checks/norm_pack.rs`.

## Reference key set
The `de` pack is the reference: every key and rule it owns must appear in a full pack
(`FULL_PACKS` in `norm_pack_tests.rs`) as a number, as `"unlimited": true` (the source sets no
limit) or as `"na": true` with the reason in `note` (the country has no such key).

## Proposed keys (not in the schema yet)
- Live loads: already `kN/m2` — `load.floor_residential`, `load.floor_office` (some packs have them).
- Floor-to-floor: `storey_height.residential`, `storey_height.office` in `m` (generic has them).
- Parking: `parking.per_dwelling` in `count`; add `parking.per_100m2_office` in `count`.
- Ventilation: needs two new units — `l/s` (outdoor air per person, key `vent.air_per_person`)
  and `1/h` (air changes, key `vent.air_changes`). Both go into `unit` in the schema and
  `unit_range` in the checker together, with a plausible range each.
