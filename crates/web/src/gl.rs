// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! WebGL2 drawing of the scene inside an egui paint callback.

use eframe::glow::{self, HasContext};

use crate::engine::Scene;

const VS: &str = r#"#version 300 es
in vec3 a_pos;
in vec3 a_nrm;
in vec4 a_col;
uniform mat4 u_mvp;
uniform int u_sel0;
uniform int u_sel1;
out vec4 v_col;
out vec3 v_nrm;
void main() {
    gl_Position = u_mvp * vec4(a_pos, 1.0);
    v_nrm = a_nrm;
    bool s = gl_VertexID >= u_sel0 && gl_VertexID < u_sel1;
    v_col = s ? vec4(1.0, 0.55, 0.1, 1.0) : a_col;
}
"#;

const FS: &str = r#"#version 300 es
precision mediump float;
in vec4 v_col;
in vec3 v_nrm;
out vec4 o_col;
void main() {
    vec3 n = normalize(v_nrm);
    float k = 0.42 + 0.45 * abs(dot(n, normalize(vec3(0.35, 0.55, 0.75))))
                   + 0.13 * abs(dot(n, normalize(vec3(-0.6, -0.2, 0.3))));
    o_col = vec4(v_col.rgb * k, 1.0);
}
"#;

pub struct Gpu {
    prog: glow::Program,
    vao: glow::VertexArray,
    bufs: [glow::Buffer; 3],
}

/// What one frame draws: vertex ranges and the selected range.
pub struct Frame {
    pub mvp: [f32; 16],
    pub ranges: Vec<(i32, i32)>,
    pub sel: (i32, i32),
    pub bg: [f32; 3],
}

unsafe fn shader(gl: &glow::Context, kind: u32, src: &str) -> Result<glow::Shader, String> {
    let s = gl.create_shader(kind)?;
    gl.shader_source(s, src);
    gl.compile_shader(s);
    if !gl.get_shader_compile_status(s) {
        return Err(gl.get_shader_info_log(s));
    }
    Ok(s)
}

impl Gpu {
    /// Uploads the scene, moved by `-origin` so f32 keeps millimetres far from the world origin.
    pub fn new(gl: &glow::Context, s: &Scene, origin: [f32; 3]) -> Result<Gpu, String> {
        unsafe {
            let prog = gl.create_program()?;
            let vs = shader(gl, glow::VERTEX_SHADER, VS)?;
            let fs = shader(gl, glow::FRAGMENT_SHADER, FS)?;
            gl.attach_shader(prog, vs);
            gl.attach_shader(prog, fs);
            gl.bind_attrib_location(prog, 0, "a_pos");
            gl.bind_attrib_location(prog, 1, "a_nrm");
            gl.bind_attrib_location(prog, 2, "a_col");
            gl.link_program(prog);
            if !gl.get_program_link_status(prog) {
                return Err(gl.get_program_info_log(prog));
            }
            gl.delete_shader(vs);
            gl.delete_shader(fs);
            let vao = gl.create_vertex_array()?;
            gl.bind_vertex_array(Some(vao));
            let pos: Vec<f32> = s
                .pos
                .iter()
                .flat_map(|p| [p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]])
                .collect();
            let nrm: Vec<f32> = s.nrm.iter().flatten().copied().collect();
            let col: Vec<u8> = s.col.iter().flatten().copied().collect();
            let mut bufs = vec![];
            for (k, (bytes, n, ty, normd)) in [
                (bytemuck_f32(&pos), 3, glow::FLOAT, false),
                (bytemuck_f32(&nrm), 3, glow::FLOAT, false),
                (&col[..], 4, glow::UNSIGNED_BYTE, true),
            ]
            .into_iter()
            .enumerate()
            {
                let b = gl.create_buffer()?;
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(b));
                gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STATIC_DRAW);
                gl.enable_vertex_attrib_array(k as u32);
                gl.vertex_attrib_pointer_f32(k as u32, n, ty, normd, 0, 0);
                bufs.push(b);
            }
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            Ok(Gpu {
                prog,
                vao,
                bufs: [bufs[0], bufs[1], bufs[2]],
            })
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.prog);
            gl.delete_vertex_array(self.vao);
            for b in self.bufs {
                gl.delete_buffer(b);
            }
        }
    }

    pub fn draw(&self, gl: &glow::Context, f: &Frame, vp: [i32; 4]) {
        unsafe {
            gl.viewport(vp[0], vp[1], vp[2], vp[3]);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(vp[0], vp[1], vp[2], vp[3]);
            gl.clear_color(f.bg[0], f.bg[1], f.bg[2], 1.0);
            gl.clear_depth_f32(1.0);
            gl.depth_mask(true);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.disable(glow::BLEND);
            gl.disable(glow::CULL_FACE);
            gl.use_program(Some(self.prog));
            // looked up per frame: a web uniform location is a JS object, not Send
            let u = |n: &str| gl.get_uniform_location(self.prog, n);
            gl.uniform_matrix_4_f32_slice(u("u_mvp").as_ref(), false, &f.mvp);
            gl.uniform_1_i32(u("u_sel0").as_ref(), f.sel.0);
            gl.uniform_1_i32(u("u_sel1").as_ref(), f.sel.1);
            gl.bind_vertex_array(Some(self.vao));
            for &(first, count) in &f.ranges {
                if count > 0 {
                    gl.draw_arrays(glow::TRIANGLES, first, count);
                }
            }
            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.disable(glow::DEPTH_TEST);
            // egui's own pass blends and does not use depth
            gl.enable(glow::BLEND);
        }
    }
}

fn bytemuck_f32(v: &[f32]) -> &[u8] {
    // f32 has no padding and u8 has alignment 1
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), std::mem::size_of_val(v)) }
}
