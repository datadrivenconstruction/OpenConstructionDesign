-- Tied arch footbridge: twin parabolic arches, hangers, deck bracing, bearings on abutments, approach ramps; sections from the structural check.
local span = param("span", "90 m", {min = "20 m", max = "250 m"})
local rise = param("rise", "15 m", {min = "5 m", max = "50 m"})
bridge{kind = "arch", span = span, rise = rise, width = "4.5 m", bays = 30, deck_z = "6 m",
       ramp_slope = 0.05, name = "arch bridge"}
audit{strict = true}
