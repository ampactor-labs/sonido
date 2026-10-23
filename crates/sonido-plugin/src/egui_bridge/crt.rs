//! The CRT post-process pass: what turns the egui frame into a lit screen.
//!
//! (All of `glow` is `unsafe`: every call here talks to the driver, with the
//! window's context current. The module allows `unsafe_code` for that alone.)
//!
//! egui draws into an offscreen texture instead of the window. Two passes
//! then put it on screen:
//!
//! 1. **Persistence** — inside the glass rect, each pixel keeps the brighter
//!    of this frame and last frame's decayed value, so moving traces leave
//!    short phosphor trails (vector-display style). Outside the glass,
//!    controls and text stay crisp.
//! 2. **Composite** — bloom everywhere (sampled from the scene's mipmaps,
//!    cheap at any size); inside the glass, barrel curvature, a little
//!    chromatic aberration, brightness-aware scanlines (bright beams bleed
//!    over the gaps, as on a real tube), an aperture-grille mask and a
//!    vignette.
//!
//! If anything fails to compile or link, the bridge renders egui directly
//! and the UI falls back to painter scanlines; nothing here is load-bearing.

#![allow(unsafe_code)]

use egui::Rect;
use glow::HasContext;

const VERTEX: &str = r"#version 150
// Fullscreen triangle from gl_VertexID; no vertex buffers.
out vec2 v_uv;
void main() {
    vec2 p = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2);
    v_uv = p;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
";

const PERSIST: &str = r"#version 150
in vec2 v_uv;
out vec4 o;
uniform sampler2D u_scene;
uniform sampler2D u_prev;
uniform vec4 u_glass;   // x0, y0, x1, y1 in uv (y up)
uniform float u_decay;
void main() {
    vec3 now = texture(u_scene, v_uv).rgb;
    bool glass = all(greaterThanEqual(v_uv, u_glass.xy)) && all(lessThanEqual(v_uv, u_glass.zw));
    vec3 prev = glass ? texture(u_prev, v_uv).rgb * u_decay : vec3(0.0);
    o = vec4(max(now, prev), 1.0);
}
";

const COMPOSITE: &str = r"#version 150
in vec2 v_uv;
out vec4 o;
uniform sampler2D u_scene;    // this frame, mipmapped (bloom source)
uniform sampler2D u_persist;  // this frame with phosphor trails
uniform vec4 u_glass;
uniform vec2 u_res;           // physical pixels
uniform float u_ppp;          // physical pixels per point
uniform float u_time;
uniform vec3 u_bezel;         // color outside the curved glass

vec3 bloom(vec2 uv) {
    vec3 b = textureLod(u_scene, uv, 1.5).rgb * 0.30;
    b += textureLod(u_scene, uv, 2.5).rgb * 0.40;
    b += textureLod(u_scene, uv, 3.5).rgb * 0.45;
    b += textureLod(u_scene, uv, 4.6).rgb * 0.40;
    return b;
}

float hash(vec2 p) { return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453); }

void main() {
    vec2 uv = v_uv;
    bool glass = all(greaterThanEqual(uv, u_glass.xy)) && all(lessThanEqual(uv, u_glass.zw));
    if (!glass) {
        vec3 c = texture(u_persist, uv).rgb + bloom(uv) * 0.32;
        o = vec4(c, 1.0);
        return;
    }

    // Barrel curvature in the glass's own -1..1 space, aspect-corrected.
    vec2 size = u_glass.zw - u_glass.xy;
    vec2 q = (uv - u_glass.xy) / size * 2.0 - 1.0;
    float aspect = (size.x * u_res.x) / (size.y * u_res.y);
    vec2 qa = vec2(q.x * aspect, q.y);
    float r2 = dot(qa, qa) / (aspect * aspect);
    vec2 warped = q * (1.0 + 0.028 * r2);
    vec2 corner = abs(warped);
    if (corner.x > 1.0 || corner.y > 1.0) { o = vec4(u_bezel, 1.0); return; }
    vec2 suv = u_glass.xy + (warped * 0.5 + 0.5) * size;

    // Chromatic aberration grows toward the edges.
    vec2 ca = (suv - (u_glass.xy + size * 0.5)) * 0.006;
    vec3 c;
    c.r = texture(u_persist, suv + ca).r;
    c.g = texture(u_persist, suv).g;
    c.b = texture(u_persist, suv - ca).b;
    c += bloom(suv) * 0.55;

    // Scanlines: dark gaps every 3 points, filled in by bright beams.
    float lum = dot(c, vec3(0.299, 0.587, 0.114));
    float period = 3.0 * u_ppp;
    float s = 0.5 + 0.5 * cos(gl_FragCoord.y * 6.28318 / period);
    c *= mix(0.62 + 0.38 * s, 1.0, clamp(lum * 0.9, 0.0, 0.75));

    // Aperture grille: RGB stripes per physical pixel column.
    int m = int(mod(gl_FragCoord.x, 3.0));
    vec3 mask = m == 0 ? vec3(1.0, 0.86, 0.86) : (m == 1 ? vec3(0.86, 1.0, 0.86) : vec3(0.86, 0.86, 1.0));
    c *= mask * 1.12;

    // Vignette plus a soft rounded edge at the curved border.
    vec2 g = warped * 0.5 + 0.5;
    float vig = pow(16.0 * g.x * g.y * (1.0 - g.x) * (1.0 - g.y), 0.22);
    float edge = smoothstep(1.0, 0.97, max(corner.x, corner.y));
    c *= vig * edge;

    // A slow refresh band and a little grain keep the glass alive.
    float band = smoothstep(0.0, 0.08, fract(g.y * 0.5 - u_time * 0.07)) * smoothstep(0.16, 0.08, fract(g.y * 0.5 - u_time * 0.07));
    c += vec3(0.018) * band;
    c += (hash(gl_FragCoord.xy + fract(u_time) * 100.0) - 0.5) * 0.025;

    o = vec4(c, 1.0);
}
";

/// One offscreen color target.
struct Target {
    fbo: glow::Framebuffer,
    tex: glow::Texture,
}

/// GPU resources for the CRT pass.
pub struct CrtPipeline {
    persist: glow::Program,
    composite: glow::Program,
    vao: glow::VertexArray,
    scene: Option<Target>,
    trails: [Option<Target>; 2],
    /// Which `trails` entry holds last frame's result.
    front: usize,
    size: (i32, i32),
}

unsafe fn compile(gl: &glow::Context, vs: &str, fs: &str) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, src);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(gl.get_shader_info_log(shader));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(gl.get_program_info_log(program));
        }
        for s in shaders {
            gl.detach_shader(program, s);
            gl.delete_shader(s);
        }
        Ok(program)
    }
}

unsafe fn make_target(gl: &glow::Context, w: i32, h: i32, mipmaps: bool) -> Result<Target, String> {
    unsafe {
        let tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w,
            h,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        let min = if mipmaps {
            glow::LINEAR_MIPMAP_LINEAR
        } else {
            glow::LINEAR
        };
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, min as i32);
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
        if mipmaps {
            gl.generate_mipmap(glow::TEXTURE_2D);
        }
        let fbo = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(tex),
            0,
        );
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        if status != glow::FRAMEBUFFER_COMPLETE {
            return Err(format!("framebuffer incomplete: {status:#x}"));
        }
        Ok(Target { fbo, tex })
    }
}

unsafe fn free_target(gl: &glow::Context, t: Option<Target>) {
    if let Some(t) = t {
        unsafe {
            gl.delete_framebuffer(t.fbo);
            gl.delete_texture(t.tex);
        }
    }
}

/// What the composite needs to know about this frame.
pub struct CrtParams {
    /// Glass rect in points, or `None` for no glass (bloom only).
    pub glass: Option<Rect>,
    /// Window size in points.
    pub logical: egui::Vec2,
    /// Physical pixels per point.
    pub ppp: f32,
    /// Seconds since start, for the refresh band and grain.
    pub time: f32,
    /// Bezel color (outside the curved glass), linear 0..1.
    pub bezel: [f32; 3],
}

#[allow(unsafe_code)]
impl CrtPipeline {
    /// Compile the shaders. Fails (and the bridge renders plainly) if the
    /// driver lacks GLSL 1.50 or anything else goes wrong.
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        // SAFETY: called with the window's GL context current.
        unsafe {
            let persist = compile(gl, VERTEX, PERSIST)?;
            let composite = compile(gl, VERTEX, COMPOSITE)?;
            let vao = gl.create_vertex_array()?;
            Ok(Self {
                persist,
                composite,
                vao,
                scene: None,
                trails: [None, None],
                front: 0,
                size: (0, 0),
            })
        }
    }

    /// Bind the offscreen scene target, (re)allocating it for `w × h`, and
    /// clear it to `clear`. egui paints into whatever is bound next.
    pub fn begin(
        &mut self,
        gl: &glow::Context,
        w: i32,
        h: i32,
        clear: [f32; 3],
    ) -> Result<(), String> {
        // SAFETY: GL context current; resources are owned by self.
        unsafe {
            if self.size != (w, h) || self.scene.is_none() {
                free_target(gl, self.scene.take());
                for t in &mut self.trails {
                    free_target(gl, t.take());
                }
                self.scene = Some(make_target(gl, w, h, true)?);
                self.trails = [
                    Some(make_target(gl, w, h, false)?),
                    Some(make_target(gl, w, h, false)?),
                ];
                for t in self.trails.iter().flatten() {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.fbo));
                    gl.clear_color(0.0, 0.0, 0.0, 1.0);
                    gl.clear(glow::COLOR_BUFFER_BIT);
                }
                self.size = (w, h);
            }
            let scene = self.scene.as_ref().expect("allocated above");
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(scene.fbo));
            gl.viewport(0, 0, w, h);
            gl.clear_color(clear[0], clear[1], clear[2], 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        Ok(())
    }

    /// Run persistence and composite onto the window's framebuffer.
    pub fn finish(&mut self, gl: &glow::Context, p: &CrtParams) {
        let (w, h) = self.size;
        let (Some(scene), [Some(_), Some(_)]) = (&self.scene, &self.trails) else {
            return;
        };
        let glass = p.glass.map_or([2.0, 2.0, -1.0, -1.0], |r| {
            let (lw, lh) = (p.logical.x.max(1.0), p.logical.y.max(1.0));
            [
                r.left() / lw,
                1.0 - r.bottom() / lh,
                r.right() / lw,
                1.0 - r.top() / lh,
            ]
        });
        let back = 1 - self.front;
        let prev_tex = self.trails[self.front].as_ref().map(|t| t.tex);
        let out = self.trails[back].as_ref().map(|t| (t.fbo, t.tex));
        let Some((out_fbo, out_tex)) = out else {
            return;
        };

        // SAFETY: GL context current; all handles are live resources of self.
        unsafe {
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.disable(glow::FRAMEBUFFER_SRGB);
            gl.bind_vertex_array(Some(self.vao));

            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(scene.tex));
            gl.generate_mipmap(glow::TEXTURE_2D);

            // Pass 1: persistence into the back trail buffer.
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(out_fbo));
            gl.viewport(0, 0, w, h);
            gl.use_program(Some(self.persist));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, prev_tex);
            self.uniforms_common(gl, self.persist, glass);
            gl.uniform_1_f32(
                gl.get_uniform_location(self.persist, "u_decay").as_ref(),
                0.74,
            );
            gl.draw_arrays(glow::TRIANGLES, 0, 3);

            // Pass 2: composite to the window.
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.viewport(0, 0, w, h);
            gl.use_program(Some(self.composite));
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(out_tex));
            self.uniforms_common(gl, self.composite, glass);
            let prog = self.composite;
            gl.uniform_1_i32(gl.get_uniform_location(prog, "u_persist").as_ref(), 1);
            gl.uniform_2_f32(
                gl.get_uniform_location(prog, "u_res").as_ref(),
                w as f32,
                h as f32,
            );
            gl.uniform_1_f32(gl.get_uniform_location(prog, "u_ppp").as_ref(), p.ppp);
            gl.uniform_1_f32(gl.get_uniform_location(prog, "u_time").as_ref(), p.time);
            gl.uniform_3_f32(
                gl.get_uniform_location(prog, "u_bezel").as_ref(),
                p.bezel[0],
                p.bezel[1],
                p.bezel[2],
            );
            gl.draw_arrays(glow::TRIANGLES, 0, 3);

            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.active_texture(glow::TEXTURE0);
        }
        self.front = back;
    }

    /// Uniforms both passes share: the scene sampler, prev/trail sampler, glass.
    unsafe fn uniforms_common(&self, gl: &glow::Context, prog: glow::Program, glass: [f32; 4]) {
        unsafe {
            gl.uniform_1_i32(gl.get_uniform_location(prog, "u_scene").as_ref(), 0);
            gl.uniform_1_i32(gl.get_uniform_location(prog, "u_prev").as_ref(), 1);
            gl.uniform_4_f32(
                gl.get_uniform_location(prog, "u_glass").as_ref(),
                glass[0],
                glass[1],
                glass[2],
                glass[3],
            );
        }
    }

    /// Release every GL resource (call with the context current).
    pub fn destroy(&mut self, gl: &glow::Context) {
        // SAFETY: GL context current; handles are owned and not used again.
        unsafe {
            free_target(gl, self.scene.take());
            for t in &mut self.trails {
                free_target(gl, t.take());
            }
            gl.delete_program(self.persist);
            gl.delete_program(self.composite);
            gl.delete_vertex_array(self.vao);
        }
    }
}
