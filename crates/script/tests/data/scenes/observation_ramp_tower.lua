-- Observation tower: a helical ramp at 1:12 wound round a steel column frame with landings.
local height = param("height", "20 m", {min = "8 m", max = "60 m"})
local r_in = param("r_in", "6 m", {min = "4.5 m", max = "20 m"})
helix_ramp{height = height, r_in = r_in, width = "2.4 m", slope_max = 1 / 12, segments = 24,
           column_every = 4, name = "ramp"}
local floors = math.floor(height / 4 + 0.5)
tower{floors = floors, floor_h = height / floors, radius = "3 m", sides = 8,
      system = "columns", core_r = 0, column = "CHS 323.9x12", beam = "I 300x150",
      name = "frame"}
audit{strict = true}
