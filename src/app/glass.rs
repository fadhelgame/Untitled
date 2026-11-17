//! Background blur ("frosted glass"): what has been painted behind a node shows blurred inside
//! the shape of that node, under its own fill.
//!
//! egui paints everything in one pass, front over back, so at the moment a node is painted the
//! framebuffer holds exactly what lies behind it. A paint callback placed right before the
//! node's fill copies that part of the framebuffer into a texture, blurs it on the GPU and
//! draws it back through the mesh of the node's shape. Nothing is kept between frames, so the
//! blur is right for whatever is behind and moves with it; a panel over another panel sees
//! that one's blur too, simply because it was painted earlier.
//!
//! The blur is a gaussian in two passes (across, then down). For a large radius the copy is
//! halved a few times on the way in, so the passes always work with a deviation of 2 to 4
//! pixels and cost the same whatever the radius. The textures all this happens in are kept and
//! shared by every blur of every frame; they only grow.

use super::blend;
use eframe::egui::{self, Rect};
use eframe::{egui_glow, glow};
use glow::HasContext as _;

/// Most halvings of the copy before it is blurred.
const MAX_LEVEL: i32 = 6;

/// What one background blur takes from the framebuffer and how it blurs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// The area that is copied, in pixels from the bottom left of the framebuffer: x, y,
    /// width, height. The shape and as far around it as the blur reaches.
    pub rect: [i32; 4],
    /// How often the copy is halved before it is blurred.
    pub level: i32,
    /// Size of the halved copy.
    pub small: [i32; 2],
    /// Deviation of the gaussian in pixels of the halved copy.
    pub sigma: f32,
}

/// Plan the background blur of a shape with the bounds `shape` (points) and a gaussian of
/// deviation `sigma` (points). `canvas`: the part of the screen that shows the document; what
/// is outside of it (panels, rulers) is not behind the shape and is not read. `None`: the
/// shape is not on the canvas, or the blur is too slight to see.
pub fn plan(shape: Rect, sigma: f32, canvas: Rect, ppp: f32, screen: [u32; 2]) -> Option<Plan> {
    let sigma = sigma * ppp;
    if !(sigma >= 0.25) || !shape.is_finite() || !shape.intersects(canvas) {
        return None;
    }
    let mut level = 0;
    while sigma / (1 << level) as f32 > 4.0 && level < MAX_LEVEL {
        level += 1;
    }
    let step = 1 << level;
    let (sw, sh) = (screen[0] as i32, screen[1] as i32);
    // Three deviations around the shape, on the grid of the halved copy (so that grid stays
    // put on the screen while the shape moves by a pixel), cut off at the canvas.
    let reach = (sigma * 3.0).ceil();
    let down = |v: f32| ((v * ppp - reach) / step as f32).floor() as i32 * step;
    let up = |v: f32| ((v * ppp + reach) / step as f32).ceil() as i32 * step;
    let edge = |v: f32| (v * ppp).round() as i32;
    let x0 = down(shape.left()).max(edge(canvas.left())).max(0);
    let x1 = up(shape.right()).min(edge(canvas.right())).min(sw);
    let y0 = down(shape.top()).max(edge(canvas.top())).max(0);
    let y1 = up(shape.bottom()).min(edge(canvas.bottom())).min(sh);
    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 0 || h <= 0 {
        return None;
    }
    // Halving averages over `step` pixels and enlarging again smooths once more: together
    // about a deviation of half a step, which the gaussian then does not have to add.
    let rest = if level > 0 { (sigma * sigma - 0.25 * (step * step) as f32).max(0.25).sqrt() } else { sigma };
    Some(Plan {
        rect: [x0, sh - y1, w, h],
        level,
        small: [(w + step - 1) / step, (h + step - 1) / step],
        sigma: rest / step as f32,
    })
}

/// A texture to draw into, with its framebuffer. It only ever grows: every blur uses the part
/// of it that it needs, from its bottom left corner.
struct Scratch {
    tex: glow::Texture,
    fbo: glow::Framebuffer,
    size: [i32; 2],
}

impl Scratch {
    unsafe fn new(gl: &glow::Context) -> Result<Self, String> {
        let (tex, fbo) = (gl.create_texture()?, gl.create_framebuffer()?);
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        for (name, value) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
        }
        Ok(Scratch { tex, fbo, size: [0, 0] })
    }

    /// Make room for `w` × `h` pixels. Sizes go up in steps, so a shape that grows a little
    /// from frame to frame (zooming in) does not get a new texture every time. The bound
    /// texture and framebuffer change.
    unsafe fn fit(&mut self, gl: &glow::Context, w: i32, h: i32) {
        if w <= self.size[0] && h <= self.size[1] {
            return;
        }
        let step = |v: i32| (v + 255) / 256 * 256;
        self.size = [self.size[0].max(step(w)), self.size[1].max(step(h))];
        gl.bind_texture(glow::TEXTURE_2D, Some(self.tex));
        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, self.size[0], self.size[1], 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(self.tex), 0);
    }
}

pub struct Gl {
    blur: glow::Program,
    blur_u: Uniforms,
    u_step: Option<glow::UniformLocation>,
    u_sigma: Option<glow::UniformLocation>,
    quad: glow::VertexArray,
    _quad_vbo: glow::Buffer,
    show: glow::Program,
    show_u: Uniforms,
    u_screen: Option<glow::UniformLocation>,
    u_px: Option<glow::UniformLocation>,
    u_rect: Option<glow::UniformLocation>,
    mesh: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    /// The copy of the framebuffer at full size, half, a quarter, ... (`down[level]`), and one
    /// more texture for the first pass of the blur.
    down: Vec<Scratch>,
    side: Scratch,
}

/// The texture a pass reads and the part of it that is in use: the size of that part and the
/// range the lookups are kept in (so nothing from outside of it is blended in), all as
/// fractions of the texture.
struct Uniforms {
    tex: Option<glow::UniformLocation>,
    part: Option<glow::UniformLocation>,
    keep: Option<glow::UniformLocation>,
}

impl Uniforms {
    unsafe fn new(gl: &glow::Context, program: glow::Program) -> Self {
        Uniforms { tex: gl.get_uniform_location(program, "u_tex"), part: gl.get_uniform_location(program, "u_part"), keep: gl.get_uniform_location(program, "u_keep") }
    }

    /// Read the part `used` (pixels) of `from`, which is bound to texture unit 0 here.
    unsafe fn set(&self, gl: &glow::Context, from: &Scratch, used: [i32; 2]) {
        let (tw, th) = (from.size[0].max(1) as f32, from.size[1].max(1) as f32);
        gl.bind_texture(glow::TEXTURE_2D, Some(from.tex));
        gl.uniform_1_i32(self.tex.as_ref(), 0);
        gl.uniform_2_f32(self.part.as_ref(), used[0] as f32 / tw, used[1] as f32 / th);
        gl.uniform_4_f32(self.keep.as_ref(), 0.5 / tw, 0.5 / th, (used[0] as f32 - 0.5) / tw, (used[1] as f32 - 0.5) / th);
    }
}

impl Gl {
    unsafe fn new(gl: &glow::Context) -> Result<Self, String> {
        let version = egui_glow::ShaderVersion::get(gl);
        let header = version.version_declaration();
        let (attr, vary_out, vary_in, tex, frag_decl, frag_out) = if version.is_new_shader_interface() {
            ("in", "out", "in", "texture", "out vec4 o_color;", "o_color")
        } else {
            ("attribute", "varying", "varying", "texture2D", "", "gl_FragColor")
        };
        let precision = "#ifdef GL_ES\nprecision highp float;\n#endif\n";
        // One pass of the gaussian: 12 taps to each side along `u_step` (three deviations at
        // the largest deviation `plan` asks for).
        let quad_vs = format!(
            "{header}\n{precision}uniform vec2 u_part;\n{attr} vec2 a_pos;\n{vary_out} vec2 v_uv;\n\
             void main() {{\n gl_Position = vec4(a_pos, 0.0, 1.0);\n v_uv = (a_pos * 0.5 + 0.5) * u_part;\n}}\n"
        );
        let blur_fs = format!(
            "{header}\n{precision}uniform sampler2D u_tex;\nuniform vec4 u_keep;\nuniform vec2 u_step;\nuniform float u_sigma;\n{vary_in} vec2 v_uv;\n{frag_decl}\n\
             void main() {{\n\
                 vec4 sum = vec4(0.0);\n float total = 0.0;\n\
                 for (int i = -12; i <= 12; i++) {{\n\
                     float x = float(i);\n\
                     float w = exp(-0.5 * x * x / (u_sigma * u_sigma));\n\
                     sum += w * {tex}(u_tex, clamp(v_uv + u_step * x, u_keep.xy, u_keep.zw));\n total += w;\n\
                 }}\n\
                 {frag_out} = sum / total;\n}}\n"
        );
        let blur = blend::compile(gl, &quad_vs, &blur_fs)?;
        let quad = gl.create_vertex_array()?;
        let quad_vbo = gl.create_buffer()?;
        gl.bind_vertex_array(Some(quad));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(quad_vbo));
        let corners: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, blend::bytes(&corners), glow::STATIC_DRAW);
        let loc = gl.get_attrib_location(blur, "a_pos").ok_or("missing attribute a_pos")?;
        gl.enable_vertex_attrib_array(loc);
        gl.vertex_attrib_pointer_f32(loc, 2, glow::FLOAT, false, 8, 0);

        // The blurred copy drawn back through the mesh of the shape: every vertex looks up
        // the place in the copy that it covers on screen. The alpha of the vertex is how much
        // of the pixel the shape covers (soft edges) times the opacity of the node.
        let show_vs = format!(
            "{header}\n{precision}uniform vec2 u_screen;\nuniform vec2 u_px;\nuniform vec4 u_rect;\nuniform vec2 u_part;\n\
             {attr} vec2 a_pos;\n{attr} vec4 a_color;\n{vary_out} vec2 v_uv;\n{vary_out} float v_cover;\n\
             void main() {{\n\
                 gl_Position = vec4(2.0 * a_pos.x / u_screen.x - 1.0, 1.0 - 2.0 * a_pos.y / u_screen.y, 0.0, 1.0);\n\
                 v_uv = (vec2(a_pos.x * u_px.x, u_px.y - a_pos.y * u_px.x) - u_rect.xy) / u_rect.zw * u_part;\n\
                 v_cover = a_color.a;\n}}\n"
        );
        let show_fs = format!(
            "{header}\n{precision}uniform sampler2D u_tex;\nuniform vec4 u_keep;\n{vary_in} vec2 v_uv;\n{vary_in} float v_cover;\n{frag_decl}\n\
             void main() {{\n {frag_out} = vec4({tex}(u_tex, clamp(v_uv, u_keep.xy, u_keep.zw)).rgb, 1.0) * v_cover;\n}}\n"
        );
        let show = blend::compile(gl, &show_vs, &show_fs)?;
        let mesh = gl.create_vertex_array()?;
        let (vbo, ebo) = (gl.create_buffer()?, gl.create_buffer()?);
        gl.bind_vertex_array(Some(mesh));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
        // `egui::epaint::Vertex`: position (2 × f32), uv (2 × f32), colour (4 × u8).
        let stride = std::mem::size_of::<egui::epaint::Vertex>() as i32;
        for (name, size, ty, normalized, offset) in [("a_pos", 2, glow::FLOAT, false, 0), ("a_color", 4, glow::UNSIGNED_BYTE, true, 16)] {
            let loc = gl.get_attrib_location(show, name).ok_or_else(|| format!("missing attribute {name}"))?;
            gl.enable_vertex_attrib_array(loc);
            gl.vertex_attrib_pointer_f32(loc, size, ty, normalized, stride, offset);
        }
        gl.bind_vertex_array(None);
        let mut down = Vec::new();
        for _ in 0..=MAX_LEVEL {
            down.push(Scratch::new(gl)?);
        }
        Ok(Gl {
            blur_u: Uniforms::new(gl, blur),
            u_step: gl.get_uniform_location(blur, "u_step"),
            u_sigma: gl.get_uniform_location(blur, "u_sigma"),
            blur,
            quad,
            _quad_vbo: quad_vbo,
            show_u: Uniforms::new(gl, show),
            u_screen: gl.get_uniform_location(show, "u_screen"),
            u_px: gl.get_uniform_location(show, "u_px"),
            u_rect: gl.get_uniform_location(show, "u_rect"),
            show,
            mesh,
            vbo,
            ebo,
            down,
            side: Scratch::new(gl)?,
        })
    }
}

unsafe fn draw(painter: &egui_glow::Painter, info: &egui::PaintCallbackInfo, shared: &blend::Shared, meshes: &[egui::Mesh], sigma: f32, canvas: Rect) {
    let gl = painter.gl();
    let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = blend::ensure(gl, &mut guard) else { return };
    if p.glass.is_none() {
        match Gl::new(gl) {
            Ok(g) => p.glass = Some(g),
            Err(e) => {
                eprintln!("background blur shader: {e}");
                return;
            }
        }
    }
    let Some(g) = p.glass.as_mut() else { return };
    let mut bounds = Rect::NOTHING;
    for v in meshes.iter().flat_map(|m| &m.vertices) {
        bounds.extend_with(v.pos);
    }
    let [w, h] = info.screen_size_px;
    let Some(plan) = plan(bounds, sigma, canvas, info.pixels_per_point, [w, h]) else { return };
    // Where egui is painting: the screen (or a framebuffer of its own). Read from, and drawn
    // into again at the end.
    let screen = std::num::NonZeroU32::new(gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) as u32).map(glow::NativeFramebuffer);
    let [x, y, pw, ph] = plan.rect;
    let level = plan.level as usize;
    gl.active_texture(glow::TEXTURE0);
    // The sizes of the copy as it is halved: [pw, ph], half of that (rounded up), ...
    let mut sizes = [[pw, ph]; MAX_LEVEL as usize + 1];
    for i in 1..=level {
        sizes[i] = [(sizes[i - 1][0] + 1) / 2, (sizes[i - 1][1] + 1) / 2];
    }
    let small = sizes[level];
    for i in level.min(1)..=level {
        g.down[i].fit(gl, sizes[i][0], sizes[i][1]);
    }
    g.side.fit(gl, small[0], small[1]);

    // 1. Copy what is behind the shape, halving it on the way: each step averages two by
    // two pixels. (The clip rectangle of egui is a scissor; it must not cut the copy or the
    // passes of the blur.)
    gl.disable(glow::SCISSOR_TEST);
    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, screen);
    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(g.down[level.min(1)].fbo));
    let [fw, fh] = sizes[level.min(1)];
    gl.blit_framebuffer(x, y, x + pw, y + ph, 0, 0, fw, fh, glow::COLOR_BUFFER_BIT, if level == 0 { glow::NEAREST } else { glow::LINEAR });
    for i in 2..=level {
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(g.down[i - 1].fbo));
        gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(g.down[i].fbo));
        gl.blit_framebuffer(0, 0, sizes[i - 1][0], sizes[i - 1][1], 0, 0, sizes[i][0], sizes[i][1], glow::COLOR_BUFFER_BIT, glow::LINEAR);
    }

    // 2. Blur across into `side`, then down, back into the copy.
    let copy = &g.down[level];
    gl.disable(glow::BLEND);
    gl.viewport(0, 0, small[0], small[1]);
    gl.use_program(Some(g.blur));
    gl.uniform_1_f32(g.u_sigma.as_ref(), plan.sigma);
    gl.bind_vertex_array(Some(g.quad));
    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(g.side.fbo));
    g.blur_u.set(gl, copy, small);
    gl.uniform_2_f32(g.u_step.as_ref(), 1.0 / copy.size[0].max(1) as f32, 0.0);
    gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(copy.fbo));
    g.blur_u.set(gl, &g.side, small);
    gl.uniform_2_f32(g.u_step.as_ref(), 0.0, 1.0 / g.side.size[1].max(1) as f32);
    gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);

    // 3. Back to the screen, through the shape.
    gl.bind_framebuffer(glow::FRAMEBUFFER, screen);
    gl.viewport(0, 0, w as i32, h as i32);
    gl.enable(glow::SCISSOR_TEST);
    gl.enable(glow::BLEND);
    gl.blend_equation(glow::FUNC_ADD);
    gl.blend_func_separate(glow::ONE, glow::ONE_MINUS_SRC_ALPHA, glow::ONE_MINUS_DST_ALPHA, glow::ONE);
    gl.use_program(Some(g.show));
    gl.uniform_2_f32(g.u_screen.as_ref(), w as f32 / info.pixels_per_point, h as f32 / info.pixels_per_point);
    gl.uniform_2_f32(g.u_px.as_ref(), info.pixels_per_point, h as f32);
    gl.uniform_4_f32(g.u_rect.as_ref(), x as f32, y as f32, pw as f32, ph as f32);
    g.show_u.set(gl, copy, small);
    gl.bind_vertex_array(Some(g.mesh));
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(g.vbo));
    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(g.ebo));
    for mesh in meshes {
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, blend::bytes(&mesh.vertices), glow::STREAM_DRAW);
        gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, blend::bytes(&mesh.indices), glow::STREAM_DRAW);
        gl.draw_elements(glow::TRIANGLES, mesh.indices.len() as i32, glow::UNSIGNED_INT, 0);
    }
    gl.bind_vertex_array(None);
    if timed() {
        gl.finish();
    }
}

/// The paint callback of a background blur: what is on screen so far, blurred with a gaussian
/// of deviation `sigma` (points), shows inside `meshes` (the shape of the node; the alpha of
/// their vertices is how strongly). `clip`: the clip rectangle of the node. `canvas`: see `plan`.
pub fn callback(shared: blend::Shared, clip: Rect, canvas: Rect, meshes: Vec<egui::Mesh>, sigma: f32) -> egui::Shape {
    let meshes = std::sync::Arc::new(meshes);
    let callback = egui_glow::CallbackFn::new(move |info, painter| unsafe {
        draw(painter, &info, &shared, &meshes, sigma, canvas);
    });
    egui::Shape::Callback(egui::PaintCallback { rect: clip, callback: std::sync::Arc::new(callback) })
}

// ---------------------------------------------------------------- bench

/// A colourful picture for the bench scenes: stripes, dots and a soft gradient, as PNG.
fn bench_picture() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(360, 240, |px, py| {
        let (fx, fy) = (px as f32 / 360.0, py as f32 / 240.0);
        let stripe = ((px + py * 2) / 24) % 2 == 0;
        let dot = ((px % 60) as f32 - 30.0).hypot((py % 60) as f32 - 30.0) < 14.0;
        let (r, g, b) = match (dot, stripe) {
            (true, _) => (250.0, 250.0, 250.0),
            (false, true) => (255.0 * fx, 90.0 + 120.0 * fy, 220.0 - 160.0 * fx),
            (false, false) => (30.0 + 60.0 * fy, 40.0 + 150.0 * fx, 120.0 + 100.0 * fy),
        };
        image::Rgba([r as u8, g as u8, b as u8, 255])
    });
    let mut png = Vec::new();
    let _ = img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
    png
}

impl super::App {
    /// `--bench` with `KANVAS_BENCH_EFFECTS`: a frame whose upper part is a busy, colourful
    /// background (gradient, shapes, a picture, text) under glass panels with a background
    /// blur: a rounded rectangle, an ellipse, a star, a frame with layers in it, a turned
    /// panel, two panels over one another, one cut off by a clipping frame, one without a
    /// fill, and one at half opacity. The lower part is a row of inner shadows: a rounded
    /// rectangle, an ellipse, a path with a hole, a text, an image, a frame with layers in
    /// it, one with a drop shadow as well, a turned one and a flipped one (sharp shadows,
    /// to show where they point), a gradient and a stroke. Result: [the frame]; its export is
    /// placed beside it by `bench_effects_finish`. `=view:x0,y0,x1,y1` shows that part of
    /// the stage (the export starts at x 1040). `=panel`: the first glass panel is selected
    /// and the panel scrolled to its Effects.
    pub(super) fn bench_effects(&mut self, ctx: &egui::Context, mode: &str, inject: &mut Vec<(u32, egui::Event)>) -> Vec<u64> {
        use super::render::PenPt;
        use crate::doc::{Gradient, Kind, Node, Shadow, Stop};
        use crate::outline::{set_contours, Contour};
        let (x, y) = (-3000.0, 32000.0);
        let (w, h) = (1000.0, 760.0);
        let stage = self.doc.alloc_id();
        let mut n = Node::new(stage, Kind::Frame, x, y);
        (n.name, n.w, n.h, n.fill) = ("Effects demo".into(), w, h, Some([246, 246, 242, 255]));
        self.doc.insert(n, None);
        let picture = self.doc.add_asset(bench_picture());
        let add = |app: &mut Self, parent: u64, kind: Kind, (nx, ny, nw, nh): (f32, f32, f32, f32), edit: &dyn Fn(&mut Node)| {
            let mut n = Node::new(app.doc.alloc_id(), kind, x + nx, y + ny);
            (n.w, n.h) = (nw, nh);
            edit(&mut n);
            let id = n.id;
            app.doc.insert(n, Some(parent));
            id
        };
        let text = |n: &mut Node, s: &str, size: f32, weight: u16, fill: [u8; 4]| {
            (n.text, n.name, n.font_size, n.weight, n.fill) = (s.into(), s.into(), size, weight, Some(fill));
        };
        let pt = |px: f32, py: f32| PenPt { x: px, y: py, hin: [0.0; 2], hout: [0.0; 2], r: None };
        let star = |cx: f32, cy: f32, outer: f32, inner: f32, points: usize| -> Vec<PenPt> {
            (0..points * 2)
                .map(|i| {
                    let (a, r) = (std::f32::consts::PI * i as f32 / points as f32 - std::f32::consts::FRAC_PI_2, if i % 2 == 0 { outer } else { inner });
                    pt(x + cx + r * a.cos(), y + cy + r * a.sin())
                })
                .collect()
        };
        let gradient = |a: [u8; 4], b: [u8; 4], angle: f32| Some(Gradient { radial: false, angle, stops: vec![Stop { pos: 0.0, color: a }, Stop { pos: 1.0, color: b }], line: None, radial_line: None });

        // --- the background of the glass panels
        add(self, stage, Kind::Rect, (0.0, 0.0, 1000.0, 440.0), &|n| (n.fill, n.gradient) = (Some([0, 0, 0, 255]), gradient([88, 28, 135, 255], [251, 146, 60, 255], 25.0)));
        for (i, (cx, cy, r, color)) in [(120.0, 90.0, 70.0, [250, 204, 21, 255]), (330.0, 150.0, 90.0, [34, 211, 238, 255]), (560.0, 80.0, 60.0, [244, 63, 94, 255]), (800.0, 170.0, 110.0, [74, 222, 128, 255]), (200.0, 330.0, 80.0, [59, 130, 246, 255]), (520.0, 330.0, 70.0, [253, 224, 71, 255]), (900.0, 370.0, 60.0, [236, 72, 153, 255])]
            .into_iter()
            .enumerate()
        {
            let kind = if i % 3 == 2 { Kind::Rect } else { Kind::Ellipse };
            add(self, stage, kind, (cx - r, cy - r, r * 2.0, r * 2.0), &|n| (n.fill, n.rotation) = (Some(color), if kind == Kind::Rect { 20.0 } else { 0.0 }));
        }
        add(self, stage, Kind::Image, (610.0, 215.0, 330.0, 220.0), &|n| (n.image, n.stroke_w) = (Some(picture), 0.0));
        add(self, stage, Kind::Text, (30.0, 150.0, 600.0, 100.0), &|n| text(n, "Frosted glass", 84.0, 800, [255, 255, 255, 255]));
        add(self, stage, Kind::Text, (36.0, 262.0, 560.0, 40.0), &|n| text(n, "sharp text behind the panels, 0123456789", 22.0, 500, [20, 20, 30, 255]));
        for i in 0..24 {
            add(self, stage, Kind::Line, (20.0 + i as f32 * 42.0, 400.0, 30.0, 30.0), &|n| (n.stroke, n.stroke_w) = (Some([255, 255, 255, 230]), 3.0));
        }

        // --- glass
        let rim = Some([255, 255, 255, 150]);
        let first = add(self, stage, Kind::Rect, (40.0, 30.0, 200.0, 130.0), &|n| {
            (n.radius, n.fill, n.bg_blur, n.stroke, n.stroke_w) = (26.0, Some([255, 255, 255, 60]), 24.0, rim, 1.5);
            // (With a glow along its inside: both effects on one layer.)
            n.inner_shadow = Some(Shadow { x: 0.0, y: 0.0, blur: 18.0, color: [255, 255, 255, 110] });
        });
        add(self, stage, Kind::Ellipse, (270.0, 30.0, 150.0, 130.0), &|n| (n.fill, n.bg_blur, n.stroke, n.stroke_w) = (Some([255, 255, 255, 50]), 14.0, rim, 1.5));
        add(self, stage, Kind::Path, (0.0, 0.0, 1.0, 1.0), &|n| {
            set_contours(n, vec![Contour { pts: star(525.0, 105.0, 90.0, 48.0, 5), closed: true }]);
            (n.fill, n.bg_blur, n.stroke, n.stroke_w) = (Some([255, 255, 255, 40]), 18.0, rim, 1.5);
        });
        let card = add(self, stage, Kind::Frame, (650.0, 30.0, 320.0, 160.0), &|n| (n.name, n.fill, n.bg_blur, n.stroke, n.stroke_w) = ("Glass frame".into(), Some([15, 23, 42, 90]), 30.0, rim, 1.0));
        add(self, card, Kind::Text, (670.0, 48.0, 280.0, 36.0), &|n| text(n, "A frame of glass", 26.0, 700, [255, 255, 255, 255]));
        add(self, card, Kind::Text, (670.0, 86.0, 280.0, 24.0), &|n| text(n, "its layers stay sharp", 15.0, 400, [226, 232, 240, 255]));
        add(self, card, Kind::Rect, (670.0, 128.0, 120.0, 40.0), &|n| (n.radius, n.fill) = (20.0, Some([255, 255, 255, 235])));
        add(self, card, Kind::Rect, (806.0, 128.0, 120.0, 40.0), &|n| (n.radius, n.fill, n.bg_blur, n.stroke, n.stroke_w) = (20.0, Some([255, 255, 255, 30]), 6.0, rim, 1.0));
        add(self, stage, Kind::Rect, (40.0, 210.0, 210.0, 110.0), &|n| (n.radius, n.fill, n.bg_blur, n.rotation, n.stroke, n.stroke_w) = (18.0, Some([255, 255, 255, 70]), 10.0, 20.0, rim, 1.5));
        // Two over one another: the upper one blurs the lower one's blur again.
        add(self, stage, Kind::Rect, (300.0, 200.0, 190.0, 140.0), &|n| (n.radius, n.fill, n.bg_blur, n.stroke, n.stroke_w) = (14.0, Some([255, 255, 255, 60]), 8.0, rim, 1.5));
        add(self, stage, Kind::Rect, (380.0, 250.0, 190.0, 140.0), &|n| (n.radius, n.fill, n.bg_blur, n.stroke, n.stroke_w) = (14.0, Some([0, 0, 0, 70]), 30.0, rim, 1.5));
        // In a frame that clips: the panel and its blur end at the edge of that frame.
        let clip = add(self, stage, Kind::Frame, (700.0, 250.0, 200.0, 150.0), &|n| (n.name, n.stroke, n.stroke_w) = ("Clip".into(), Some([255, 255, 255, 255]), 2.0));
        add(self, clip, Kind::Ellipse, (640.0, 270.0, 180.0, 180.0), &|n| (n.fill, n.bg_blur) = (Some([255, 255, 255, 50]), 16.0));
        add(self, clip, Kind::Rect, (830.0, 230.0, 120.0, 90.0), &|n| (n.fill, n.bg_blur, n.radius) = (Some([255, 0, 80, 60]), 12.0, 12.0));
        // No fill at all, and half opacity (half blurred).
        add(self, stage, Kind::Ellipse, (585.0, 150.0, 60.0, 60.0), &|n| (n.bg_blur, n.stroke, n.stroke_w) = (10.0, Some([255, 255, 255, 255]), 2.0));
        add(self, stage, Kind::Rect, (30.0, 345.0, 240.0, 80.0), &|n| (n.radius, n.fill, n.bg_blur, n.opacity) = (40.0, Some([255, 255, 255, 120]), 20.0, 0.5));

        // --- inner shadows
        let soft = |sx: f32, sy: f32, blur: f32, a: u8| Some(Shadow { x: sx, y: sy, blur, color: [0, 0, 0, a] });
        let sharp = Some(Shadow { x: 8.0, y: 10.0, blur: 0.0, color: [220, 38, 38, 255] });
        add(self, stage, Kind::Rect, (30.0, 470.0, 120.0, 110.0), &|n| (n.radius, n.fill, n.inner_shadow) = (24.0, Some([253, 224, 71, 255]), soft(0.0, 6.0, 14.0, 130)));
        add(self, stage, Kind::Ellipse, (170.0, 470.0, 120.0, 110.0), &|n| (n.fill, n.inner_shadow) = (Some([96, 165, 250, 255]), soft(6.0, 6.0, 12.0, 150)));
        add(self, stage, Kind::Path, (0.0, 0.0, 1.0, 1.0), &|n| {
            set_contours(n, vec![Contour { pts: star(370.0, 525.0, 62.0, 50.0, 8), closed: true }, Contour { pts: star(370.0, 525.0, 26.0, 20.0, 4), closed: true }]);
            (n.fill, n.inner_shadow) = (Some([74, 222, 128, 255]), soft(0.0, 5.0, 8.0, 170));
        });
        add(self, stage, Kind::Text, (450.0, 462.0, 260.0, 110.0), &|n| {
            text(n, "Inset", 96.0, 800, [244, 114, 182, 255]);
            n.inner_shadow = soft(2.0, 4.0, 4.0, 170);
        });
        add(self, stage, Kind::Image, (720.0, 470.0, 150.0, 110.0), &|n| (n.image, n.stroke_w, n.radius, n.inner_shadow) = (Some(picture), 0.0, 18.0, soft(0.0, 0.0, 30.0, 220)));
        let well = add(self, stage, Kind::Frame, (880.0, 470.0, 100.0, 110.0), &|n| (n.name, n.fill, n.inner_shadow) = ("Well".into(), Some([226, 232, 240, 255]), soft(0.0, 8.0, 16.0, 150)));
        add(self, well, Kind::Rect, (895.0, 500.0, 70.0, 30.0), &|n| (n.radius, n.fill) = (8.0, Some([37, 99, 235, 255])));
        add(self, well, Kind::Text, (895.0, 540.0, 80.0, 20.0), &|n| text(n, "on top", 15.0, 600, [30, 41, 59, 255]));
        // Second row: with a drop shadow, turned, flipped (an image and a rectangle), a
        // gradient under a white inner glow, a stroke over the shadow, and a star with rounded corners.
        add(self, stage, Kind::Rect, (40.0, 620.0, 110.0, 100.0), &|n| {
            (n.radius, n.fill, n.inner_shadow) = (20.0, Some([255, 255, 255, 255]), soft(0.0, 3.0, 8.0, 110));
            n.shadow = Some(Shadow { x: 0.0, y: 10.0, blur: 20.0, color: [0, 0, 0, 90] });
        });
        add(self, stage, Kind::Rect, (190.0, 625.0, 100.0, 90.0), &|n| (n.fill, n.inner_shadow) = (Some([226, 232, 240, 255]), sharp));
        add(self, stage, Kind::Rect, (330.0, 625.0, 100.0, 90.0), &|n| (n.fill, n.inner_shadow, n.rotation) = (Some([226, 232, 240, 255]), sharp, 30.0));
        add(self, stage, Kind::Image, (470.0, 625.0, 120.0, 90.0), &|n| (n.image, n.stroke_w, n.inner_shadow, n.flip_h) = (Some(picture), 0.0, sharp, true));
        add(self, stage, Kind::Rect, (620.0, 625.0, 100.0, 90.0), &|n| {
            (n.fill, n.gradient, n.radius) = (Some([0, 0, 0, 255]), gradient([14, 165, 233, 255], [99, 102, 241, 255], 90.0), 45.0);
            n.inner_shadow = Some(Shadow { x: 0.0, y: 0.0, blur: 24.0, color: [255, 255, 255, 230] });
        });
        add(self, stage, Kind::Ellipse, (750.0, 625.0, 100.0, 90.0), &|n| (n.fill, n.inner_shadow, n.stroke, n.stroke_w) = (Some([250, 204, 21, 255]), soft(0.0, 10.0, 6.0, 200), Some([30, 41, 59, 255]), 6.0));
        add(self, stage, Kind::Path, (0.0, 0.0, 1.0, 1.0), &|n| {
            set_contours(n, vec![Contour { pts: star(920.0, 670.0, 52.0, 24.0, 5), closed: true }]);
            (n.fill, n.inner_shadow, n.radius) = (Some([251, 146, 60, 255]), soft(-4.0, 6.0, 6.0, 190), 6.0);
        });

        let part: Vec<f32> = mode.strip_prefix("view:").map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect()).unwrap_or_default();
        let view = if part.len() == 4 { (x + part[0], y + part[1], x + part[2], y + part[3]) } else { (x - 20.0, y - 20.0, x + w * 2.0 + 60.0, y + h + 20.0) };
        self.zoom_to(view, 32.0);
        self.fit_pending = false;
        self.sel.clear();
        if mode == "panel" {
            self.sel = vec![first];
            let p = egui::pos2(ctx.screen_rect().right() - 150.0, 500.0);
            let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -900.0), modifiers: Default::default() };
            inject.extend([(200, egui::Event::PointerMoved(p)), (204, wheel.clone()), (212, wheel), (224, egui::Event::PointerMoved(self.canvas_rect.center_top() + egui::vec2(0.0, 40.0)))]);
        }
        vec![stage]
    }

    /// The second half of `bench_effects`, once the fonts are loaded: the frame is exported
    /// (PNG, SVG and PDF beside the screenshot) and the PNG is placed right of the frame.
    pub(super) fn bench_effects_finish(&mut self, ctx: &egui::Context, stage: u64, out: &std::path::Path) {
        use crate::doc::{Kind, Node};
        let start = std::time::Instant::now();
        let Some((x, y, w, h)) = self.doc.find(stage).map(|f| (f.x, f.y, f.w, f.h)) else { return };
        if let Some((svg, sw, sh)) = crate::export::to_svg(&self.doc, Some(stage), &mut self.fonts, ctx) {
            let made = start.elapsed().as_secs_f32() * 1000.0;
            let _ = crate::export::save_raster(&svg, sw, sh, 2.0, false, &out.with_file_name("bench-effects-export.png"));
            let _ = std::fs::write(out.with_file_name("bench-effects-export.svg"), &svg);
            let _ = crate::export::save_pdf(&[(svg.clone(), sw, sh)], &out.with_file_name("bench-effects-export.pdf"));
            if let Ok((png, _, _)) = crate::export::render_png(&svg, sw, sh, 3.0) {
                let asset = self.doc.add_asset(png);
                let mut n = Node::new(self.doc.alloc_id(), Kind::Image, x + w + 40.0, y);
                (n.name, n.w, n.h, n.image, n.stroke_w) = ("Effects export".into(), w, h, Some(asset), 0.0);
                self.doc.insert(n, None);
            }
            eprintln!("EFFECTS svg made in {made:.1} ms ({} kB, {} backdrop pictures)", svg.len() / 1024, svg.matches(r#"<clipPath id="bc"#).count());
        }
        eprintln!("EFFECTS exported in {:.1} ms", start.elapsed().as_secs_f32() * 1000.0);
    }

    /// `--bench` with `KANVAS_BENCH_GLASS=<n>`: `n` glass panels (background blur) over the
    /// frames of the stress test, in a grid around the middle of the view, so the usual run of
    /// zooming and panning measures what they cost. While this is set, every blur waits for
    /// the GPU to finish, so its time is part of the frame time that is reported.
    pub(super) fn bench_glass(&mut self, count: usize) {
        use crate::doc::{Kind, Node};
        let c = self.view.to_world(self.canvas_rect.min, self.canvas_rect.center());
        let cols = count.clamp(1, 5);
        let rows = count.div_ceil(cols);
        for i in 0..count {
            let (col, row) = ((i % cols) as f32 - (cols as f32 - 1.0) * 0.5, (i / cols) as f32 - (rows as f32 - 1.0) * 0.5);
            let mut n = Node::new(self.doc.alloc_id(), Kind::Rect, c.x + col * 520.0 - 240.0, c.y + row * 360.0 - 160.0);
            (n.name, n.w, n.h, n.radius, n.fill, n.bg_blur) = (format!("Glass {}", i + 1), 480.0, 320.0, 40.0, Some([255, 255, 255, 50]), 40.0);
            self.doc.insert(n, None);
        }
    }
}

/// `KANVAS_BENCH_GLASS` is set: see `App::bench_glass`.
fn timed() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("KANVAS_BENCH_GLASS").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn the_copy_covers_the_shape_and_the_reach_of_the_blur() {
        let canvas = Rect::from_min_max(pos2(50.0, 40.0), pos2(1000.0, 700.0));
        let shape = Rect::from_min_max(pos2(200.0, 100.0), pos2(400.0, 300.0));
        // A small blur: nothing is halved, three deviations around the shape. Rows count from
        // the bottom of the framebuffer.
        let p = plan(shape, 2.0, canvas, 1.0, [1200, 800]).unwrap();
        assert_eq!(p, Plan { rect: [194, 800 - 306, 212, 212], level: 0, small: [212, 212], sigma: 2.0 });
        // A large one: halved until the deviation is at most 4 pixels, on a grid of that step.
        let p = plan(shape, 20.0, canvas, 1.0, [1200, 800]).unwrap();
        assert_eq!((p.level, p.rect), (3, [136, 800 - 360, 328, 320]));
        assert_eq!(p.small, [41, 40]);
        assert!(p.sigma > 2.0 && p.sigma <= 2.5, "{}", p.sigma);
        // Moving the shape by a pixel does not move that grid.
        let moved = plan(shape.translate(egui::vec2(1.0, 0.0)), 20.0, canvas, 1.0, [1200, 800]).unwrap();
        assert_eq!(moved.rect, p.rect);
        // Pixels, not points; and never more halvings than the textures have.
        let hi = plan(shape, 2.0, canvas, 2.0, [2400, 1600]).unwrap();
        assert_eq!((hi.level, hi.rect, hi.sigma), (0, [388, 1600 - 612, 424, 424], 4.0));
        assert_eq!(plan(shape, 5000.0, canvas, 1.0, [1200, 800]).unwrap().level, MAX_LEVEL);
        // Cut off at the canvas: the panels around it are not behind the shape.
        let edge = Rect::from_min_max(pos2(40.0, 30.0), pos2(120.0, 90.0));
        let p = plan(edge, 2.0, canvas, 1.0, [1200, 800]).unwrap();
        assert_eq!(p.rect, [50, 800 - 96, 76, 56]);
        // Off the canvas, or too slight to see: nothing to do.
        assert_eq!(plan(shape.translate(egui::vec2(2000.0, 0.0)), 8.0, canvas, 1.0, [1200, 800]), None);
        assert_eq!(plan(shape, 0.1, canvas, 1.0, [1200, 800]), None);
        assert_eq!(plan(Rect::NOTHING, 8.0, canvas, 1.0, [1200, 800]), None);
    }
}
