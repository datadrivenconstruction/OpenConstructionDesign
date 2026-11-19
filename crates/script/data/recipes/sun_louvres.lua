-- Sun louvres: horizontal blades tilted for the sun altitude between two posts.
local length = param("length", "12 m", {min = "2 m", max = "30 m"})
local altitude = param("altitude", "50 deg", {min = "5 deg", max = "85 deg"})
fins{from = vec3(0, 0, 0), to = vec3(length, 0, 0), height = "6 m", direction = "horizontal",
     spacing = "0.4 m", depth = "0.3 m", mode = "sun", sun = {azimuth = "180 deg", altitude = altitude},
     max_rotation = "60 deg", name = "louvres"}
audit{strict = true}
