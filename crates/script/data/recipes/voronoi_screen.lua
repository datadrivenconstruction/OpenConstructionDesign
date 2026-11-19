-- Voronoi screen: perforated weathering-steel plates between posts, openings widest near the gate.
local length = param("length", "14.4 m", {min = "3 m", max = "60 m"})
local seed = param("seed", 7)
perforated_panel{from = vec3(0, 0, 0), to = vec3(length, 0, 0), count = 4,
                 height = "3.2 m", cells = 30, seed = seed,
                 attractor = vec3(length / 2, 0, 1.2), name = "screen"}
audit{strict = true}
