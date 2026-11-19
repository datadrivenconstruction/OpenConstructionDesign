-- Tapered twisting tower: inclined perimeter columns turning 45 degrees, slabs round a core.
local floors = param("floors", 30, {min = 6, max = 90})
tower{floors = floors, floor_h = "3.6 m", radius = "15 m", sides = 10, taper = 0.6,
      twist = "45 deg", system = "columns", name = "tower"}
audit{strict = true}
