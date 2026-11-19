-- Twisted diagrid tower: steel diagonals in staggered rows, ring beams on every floor, slabs round a concrete core.
local floors = param("floors", 24, {min = 4, max = 80})
local radius = param("radius", "16 m", {min = "8 m", max = "40 m"})
local twist = param("twist", "90 deg", {min = "-180 deg", max = "180 deg"})
local taper = param("taper", 0.8, {min = 0.5, max = 1.2})
tower{floors = floors, floor_h = "4 m", radius = radius, sides = 12, twist = twist,
      taper = taper, system = "diagrid", module = 2, diagonal = "CHS 508x25",
      beam = "I 500x200", name = "tower"}
audit{strict = true}
