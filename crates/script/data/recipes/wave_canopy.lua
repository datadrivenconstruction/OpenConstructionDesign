-- Wave canopy: a triangulated gridshell over a sine surface on edge columns, glazed.
local sx = param("length", "40 m", {min = "10 m", max = "120 m"})
local sy = param("width", "16 m", {min = "6 m", max = "40 m"})
surface_grid{surface = surf.wave{size_x = sx, size_y = sy, height = "7 m", amplitude = "2 m",
             waves_x = 1.5, waves_y = 0.5}, nu = 20, nv = 8, pattern = "tri",
             edge_columns = 4, panels = "12 mm", name = "canopy"}
audit{strict = true}
