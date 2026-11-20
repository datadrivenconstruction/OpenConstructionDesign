# Bridge protocol v1

A **bridge** is a separate executable that reads some file format and streams neutral building
data to the editor. The editor never links it. This document is the complete, normative
specification: a bridge implements it on its own. The editor's codec, crate `ddc-bridge-proto`
(`crates/bridge-proto`), is AGPL-3.0-only like the rest of the editor; closed bridges must
not use it or any other editor crate. Reference bridge: `ddc-bridge-sample`
(`crates/io/src/bin/ddc-bridge-sample.rs`).

## 1. Installation, package manifest, trust
```
<bridges dir>/<name>/bridge.json          package manifest (below)
<bridges dir>/<name>/bridge.json.minisig  minisign signature of the exact bridge.json bytes
<bridges dir>/<name>/<every file listed in bridge.json>
```
`<bridges dir>` = `DDC_BRIDGE_DIR`, else `%APPDATA%\ddc-editor\bridges`, else
`$XDG_CONFIG_HOME` (or `~/.config`)`/ddc-editor/bridges`.

Manifest (UTF-8 JSON, at most 1 MiB):
```json
{"name": "sample", "version": "1.4.0", "exe": "x.exe", "protocol": 1, "formats": ["rvt", "rfa"],
 "files": [{"path": "x.exe", "sha256": "<64 lower-case hex>", "size": 123456},
           {"path": "helper.dll", "sha256": "<64 lower-case hex>", "size": 4096}]}
```
- `name`: 1-64 of `[A-Za-z0-9._-]`. `version`: 1-4 dot-separated numbers. `formats`: no dot,
  compared lower case. `exe` must be one of `files`.
- `files`: every file of the package, bare names only (no folders, no `..`, no device names),
  unique ignoring case, at most 256 files, each at most 1 GiB, total at most 2 GiB. Any other
  file in the package folder makes the package untrusted. Libraries the bridge loads must be listed.
- Signature trusted comment, exactly:
  `ddc-bridge name=<name> version=<version> formats=<formats joined by ",">` (formats lower
  case, in manifest order). A mismatch makes the package untrusted.

Trust: only keys compiled into the editor (official release keys) and keys the user confirmed
in developer mode (stored in `<config dir>/developer_keys.txt`, never in the bridges dir;
`DDC_BRIDGE_DIR` does not affect keys). The editor also carries revoked key ids, revoked
versions and a minimum version per bridge name. Discovery checks the manifest signature and the
folder listing. Before every run, off the UI thread and with a time limit, the listed files are
copied into a new per-run folder only the current user can open, opened deny-write, hashed and
compared with the manifest. The copy runs, never the installed files; the folder is removed after
the process exits. Only regular local files are accepted (no links, pipes, devices, network paths).

### Signing a package (release tooling)
The release tooling of a bridge must: (1) assemble the package folder with
exactly the files to ship; (2) write `bridge.json` with `files` = every file, its SHA-256
(lower-case hex) and byte size; (3) sign the bytes of `bridge.json` with the release key
(minisign, prehashed "ED") using the trusted comment `ddc-bridge name=… version=… formats=…`;
(4) write `bridge.json.minisig` next to it; (5) fail if any other file is present. Per-executable
`<exe>.minisig` files are no longer read. `ddc_io::bridge::package::manifest_for` produces the
same manifest and can serve as a reference.

## 2. Process
```
<bridge>.exe --protocol 1 --input <file> [--view <name>]
```
- stdout: the frame stream (below). stderr: free text, the host keeps the last 8 KiB for errors.
- stdin: the host may write the line `cancel\n` or close stdin; both mean *stop now*. The bridge
  exits within 3 s, else it is killed. After writing End the bridge exits with code 0.
- Environment: only `SystemRoot`, `TEMP`, `TMP`, `LANG`, and `PATH` = system folders; working
  directory = the private copy folder (holds only signed files).
- Unknown arguments must be ignored. Unsupported `--protocol` → exit code 2.
- Host limits: no frame for 120 s → killed (hang); total time limit; totals per import (default
  2 GiB of stream, 5 M elements, 200 M vertices, configurable) → `too large`; any error → kill.

## 3. Stream
```
preamble: "DDCB" (4 bytes) + u16 protocol version (= 1)
frame:    u32 length N (1 ≤ N ≤ 16 MiB) + N bytes: u8 tag + payload
```
All integers little-endian. Primitive encodings:

| name | encoding |
|---|---|
| `f64` | IEEE-754 binary64; NaN/∞ are rejected |
| `str` | u32 byte length (≤ 64 KiB) + UTF-8 |
| `vec<T>` | u32 count + items; count × min item size must fit in the rest of the frame |
| `opt<T>` | u8 0 (absent) / 1 + T |
| `bool` | u8 0/1 (other values rejected) |
| `p2` | f64 x, f64 y |
| `guid` | `str`, 1-256 bytes, stable across exports of the same source element |
| `value` | u8 tag + 0: bool · 1: i64 · 2: f64 · 3: str |

Compatibility: within major version 1, fields are only appended at the end of a payload, and
readers ignore trailing bytes. New frame tags may appear; the host skips unknown tags and
reports them. A breaking change bumps the preamble version.

## 4. Order
`Hello` first, `End` last. `Units` before any geometry. Levels, Materials and Types before the
Elements that reference them. A host wall must precede its openings. Properties reference any
earlier guid. Progress and Warnings may appear anywhere after Hello. Elements are sent in many
frames (recommended ≤ 4096 elements per frame) so 1 M elements stream with flat memory: the host
reads through a bounded queue, so a fast bridge is throttled by the importer.

## 5. Frames
Lengths are in source units × `Units.length_to_m` = metres; angles are radians. Plan
coordinates are x, y; elevations are z.

| tag | frame | payload |
|---|---|---|
| 1 | Hello | u16 protocol, str bridge name, str version, vec<str> formats (no dot, lower case), vec<str> capabilities |
| 2 | Progress | u64 done, u64 total (0 = unknown), str phase |
| 3 | Units | f64 length_to_m (0 < x ≤ 1000); every scaled value must stay finite |
| 4 | Levels | vec<{guid, str name, f64 elevation}> |
| 5 | Grids | vec<{guid, str name, p2 a, p2 b}> |
| 6 | Materials | vec<{guid, str name, u32 color 0xRRGGBBAA}> |
| 7 | Types | vec<type> |
| 8 | Elements | vec<element> |
| 9 | Properties | vec<{guid owner, str key, value, str unit ("" = none)}> |
| 10 | Warnings | vec<{str code, str message, opt<guid> element}> |
| 11 | End | u64 elements (total sent; host checks), u64 warnings |
| 12 | WallTops | vec<{guid wall, vec<p2> (along, height)}>: top edge of a straight wall |
| 13 | SlabSlopes | vec<{guid slab, p2 point, p2 gradient (dz/dx, dz/dy)}>: before the slab's Elements frame; the slab's offset is its top at `point` (minor ≥ 2) |
| 14 | CurtainWalls | vec<{guid, guid level, str name, curve axis, f64 base_offset, f64 height, vec<f64> u, vec<f64> v, section interior, section border, vec<panel>}> (minor ≥ 2) |
| 28 | Prisms | vec<{guid, guid level, str name, str category, str material, f64[3] o, f64[3] x, f64[3] y, vec<p2> outline, f64 depth}>: one closed extrusion; `outline` in the plane of orthonormal `x`, `y` about the world point `o`, along `x × y` over `depth`; empty `material` = none named |

Minor version: the host passes `--minor N` (absent = 0). Frames 13 and 14 are sent only when
N ≥ 2, frame 28 only when N ≥ 6; an older host gets the same elements as generic meshes.

`u` / `v`: interior grid lines, distance along the axis / above the base. `section`: u8 0 none ·
1 rect (f64 width in the wall plane, f64 depth) · 2 circle (f64 diameter). `panel`: u32 i, u32 j
(cell column / row from the axis start and the base), u8 kind (0 glazed, 1 solid, 2 door,
3 window, 4 empty), str name, opt<guid> family_type, bool swing_side, bool swing_hinge. A cell
without a panel is glazed.

### type (u8 kind + fields)
| kind | | fields |
|---|---|---|
| 0 | wall type | guid, str name, vec<{guid material, f64 thickness}>: layers from the left face (looking from axis start to end) to the right |
| 1 | slab type | guid, str name, vec<layer>: top to bottom |
| 2 | family type | guid, str category ("door", "window", any neutral word), str name, vec<{str key, value}> |

### element (u8 kind + fields)
| kind | element | fields |
|---|---|---|
| 0 | wall | guid, guid level, guid wall_type, curve axis, f64 base_offset, top, opt<f64> offset0 |
| 1 | opening | guid, guid host, u8 kind (0 door, 1 window, 2 void), opt<guid> family_type, f64 along, f64 width, f64 height, f64 sill, bool swing_side, bool swing_hinge |
| 2 | slab | guid, guid level, guid slab_type, f64 offset (top above level), vec<p2> outer, vec<vec<p2>> holes |
| 3 | room | guid, guid level, str name, str number, f64 height, vec<p2> boundary, opt<p2> point (inside) |
| 4 | generic | guid, opt<guid> level, str category, str name, vec<[f64;3]> positions, vec<u32> triangle indices |

- `curve`: u8 0 line {p2 a, p2 b} · 1 arc through three points {p2 a, p2 mid, p2 b}.
- `top`: u8 0 unconnected {f64 height} · 1 to level {guid level, f64 offset}. Height =
  top level elevation + offset − base level elevation − base_offset.
- `offset0`: left offset of the first layer from the axis; absent = wall centred on the axis.
- Opening `along`: distance along the host axis from its start to the near jamb (arc length on
  arcs). `swing_side` false = leaf opens to the left of the axis; `swing_hinge` false = hinges at
  the jamb nearer the axis start.
- Rings may be open or closed (a repeated last point is dropped); orientation is normalised.

## 6. Host mapping (ddc-io)
- One import = one undoable model operation. On any error (protocol, data, crash, hang,
  cancel, model) the transaction rolls back: the model is unchanged.
- Walls, openings, slabs, rooms, levels, wall and slab types become native objects. Arc walls
  become chord segments within 5 mm sagitta (guids `g`, `g#1`, …). Generic elements are stored
  as meshes with category; properties and family type parameters (`type.<key>`) go to `prop`.
- Grids are reported as skipped. Levels and types with an existing equal name/elevation or a
  known guid are reused; name clashes get a ` (2)` suffix.
- Guid namespace: source = Hello bridge name + canonical path of the input file; a guid is
  unique per (source, guid), so two files never overwrite each other's elements. A guid already
  linked to a live object of another kind is reported (`guid collision`), never overwritten.
- Re-import of the same source: an element whose guid was imported before (and still exists) is
  kept (`unchanged`) or updated in place (`updated`); new guids are added; guids of this source
  that are absent are reported as `missing`, never deleted.
- Report: counts per kind, native wall segments, unchanged, skipped categories, warnings.

## 7. Errors the host reports
untrusted (signature, manifest, unsigned file, hash, policy, verify timeout) · spawn failed · protocol (bad magic/version, oversize, malformed, first frame not
Hello, no End, End count mismatch) · data (unknown level/type/host, bad sizes or indices) ·
crashed (exit code + stderr tail) · timeout · cancelled · too large · model.
