-- Hypar umbrellas: thin shells of four hyperbolic paraboloids, each umbrella on one column.
local nx = param("nx", 3, {min = 1, max = 10})
local ny = param("ny", 2, {min = 1, max = 10})
hypar_umbrella{nx = nx, ny = ny, size = "10 m", height = "6 m", rise = "2 m",
               divisions = 5, shell = "60 mm", name = "umbrellas"}
audit{strict = true}
