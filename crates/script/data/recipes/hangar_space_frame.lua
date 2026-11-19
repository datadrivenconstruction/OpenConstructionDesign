-- Hangar: 80 m clear span double-layer space frame on perimeter columns, crane runway and girder, sliding gate leaves; sections from the structural check.
local nx = param("nx", 20, {min = 8, max = 40})
local ny = param("ny", 16, {min = 8, max = 30})
space_frame{nx = nx, ny = ny, bay = "5 m", depth = "4.5 m", height = "28 m", columns = "perimeter",
            column_every = 2, crane_z = "14 m", gates = 4, gate_h = "20 m", name = "hangar"}
audit{strict = true}
