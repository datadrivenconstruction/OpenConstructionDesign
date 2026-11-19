-- Stadium roof: cantilever trusses round an elliptic opening on columns with back stays, purlins, front ring, cover, three raked seating tiers; sections from the structural check.
local a = param("a", "60 m", {min = "30 m", max = "150 m"})
local b = param("b", "42 m", {min = "20 m", max = "120 m"})
stadium_roof{a = a, b = b, depth = "24 m", bays = 32, divisions = 5, height = "28 m",
             truss_depth = "6 m", front_depth = "1.5 m", rake = 0.08, bowl = true, name = "roof"}
audit{strict = true}
