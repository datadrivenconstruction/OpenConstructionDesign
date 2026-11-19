-- Space frame hall: square-on-square double-layer grid on perimeter columns.
local nx = param("nx", 12, {min = 2, max = 40})
local ny = param("ny", 8, {min = 2, max = 40})
space_frame{nx = nx, ny = ny, bay = "3 m", height = "9 m", columns = "perimeter",
            column_every = 3, name = "space frame"}
audit{strict = true}
