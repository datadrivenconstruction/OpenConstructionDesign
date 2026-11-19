-- Fin facade: vertical aluminium fins turned towards an attractor point between frame rails.
local length = param("length", "36 m", {min = "6 m", max = "120 m"})
local height = param("height", "16 m", {min = "3 m", max = "60 m"})
fins{from = vec3(0, 0, 0), to = vec3(length, 0, 0), height = height, spacing = "0.75 m",
     depth = "0.45 m", mode = "attractor", attractor = vec3(length / 2, -5, height / 2),
     max_rotation = "70 deg", name = "fins"}
audit{strict = true}
