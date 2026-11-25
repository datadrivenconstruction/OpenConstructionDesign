-- Lattice on a surface: a diagrid of members over a surf.* shape (here a dome with an oculus) and an edge ring; swap the surface for a vault, hypar, loft or wave.
local radius = param("radius", "18 m", {min = "8 m", max = "40 m"})
local rise = param("rise", "7 m", {min = "3 m", max = "20 m"})
surface_grid{surface = surf.dome{radius = radius, rise = rise, oculus = "3 m"},
  nu = 32, nv = 8, closed_u = true, pattern = "diagrid", member = "rect 120x240",
  edge = "rect 200x360", material = "glulam GL28h", name = "lattice"}
audit{strict = true}
