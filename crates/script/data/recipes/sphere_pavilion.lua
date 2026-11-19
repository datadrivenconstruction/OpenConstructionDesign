-- Sphere pavilion: a near-full sphere of triangulated tubes on a small base ring.
local base = param("base", "4 m", {min = "2 m", max = "10 m"})
local height = param("height", "14 m", {min = "6 m", max = "40 m"})
surface_grid{surface = surf.dome{radius = base, rise = height, oculus = "1.5 m"},
             nu = 24, nv = 12, closed_u = true, pattern = "tri", member = "CHS 114.3x6",
             edge = "CHS 219.1x10", name = "sphere"}
audit{strict = true}
