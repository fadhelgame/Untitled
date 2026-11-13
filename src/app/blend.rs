//! Blend mode. egui selalu menggambar dengan alpha blending biasa, jadi node yang memakai
//! blend mode digambar lewat callback OpenGL sendiri: shader sederhana (tekstur × warna vertex)
//! dengan fungsi blend yang diganti. Hanya mode yang bisa dibuat dari fungsi blend bawaan GPU
//! yang tersedia (tanpa membaca isi framebuffer).

use eframe::egui::{self, Rect};
use eframe::{egui_glow, glow};
use glow::HasContext as _;
use std::sync::{Arc, Mutex};

pub const NAMES: [&str; 7] = ["Normal", "Darken", "Multiply", "Lighten", "Screen", "Plus lighter", "Exclusion"];
/// Nilai `mix-blend-mode` untuk export SVG.
pub const CSS: [&str; 7] = ["normal", "darken", "multiply", "lighten", "screen", "plus-lighter", "exclusion"];

pub struct Program {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    u_screen: Option<glow::UniformLocation>,
    u_sampler: Option<glow::UniformLocation>,
    u_white: Option<glow::UniformLocation>,
    /// Perlengkapan clipping mask, dibuat saat pertama dipakai.
    mask: Option<MaskGl>,
}

/// Clipping mask: alas dan isi digambar ke dua tekstur seukuran layar, lalu digabung
/// (isi × alpha alas) ke layar.
struct MaskGl {
    program: glow::Program,
    vao: glow::VertexArray,
    _vbo: glow::Buffer,
    u_content: Option<glow::UniformLocation>,
    u_mask: Option<glow::UniformLocation>,
    /// [alas, isi]
    fbo: [glow::Framebuffer; 2],
    tex: [glow::Texture; 2],
    size: [i32; 2],
}

unsafe fn compile(gl: &glow::Context, vs: &str, fs: &str) -> Result<glow::Program, String> {
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
    for shader in shaders {
        gl.detach_shader(program, shader);
        gl.delete_shader(shader);
    }
    Ok(program)
}

impl MaskGl {
    unsafe fn new(gl: &glow::Context) -> Result<Self, String> {
        let version = egui_glow::ShaderVersion::get(gl);
        let header = version.version_declaration();
        let (attr, vary_out, vary_in, tex, frag_decl, frag_out) = if version.is_new_shader_interface() {
            ("in", "out", "in", "texture", "out vec4 o_color;", "o_color")
        } else {
            ("attribute", "varying", "varying", "texture2D", "", "gl_FragColor")
        };
        let vs = format!(
            "{header}\n#ifdef GL_ES\nprecision mediump float;\n#endif\n\
             {attr} vec2 a_pos;\n{vary_out} vec2 v_uv;\n\
             void main() {{\n gl_Position = vec4(a_pos, 0.0, 1.0);\n v_uv = a_pos * 0.5 + 0.5;\n}}\n"
        );
        let fs = format!(
            "{header}\n#ifdef GL_ES\nprecision mediump float;\n#endif\n\
             uniform sampler2D u_content;\nuniform sampler2D u_mask;\n{vary_in} vec2 v_uv;\n{frag_decl}\n\
             void main() {{\n {frag_out} = {tex}(u_content, v_uv) * {tex}(u_mask, v_uv).a;\n}}\n"
        );
        let program = compile(gl, &vs, &fs)?;
        let vao = gl.create_vertex_array()?;
        let vbo = gl.create_buffer()?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes(&quad), glow::STATIC_DRAW);
        let loc = gl.get_attrib_location(program, "a_pos").ok_or("missing attribute a_pos")?;
        gl.enable_vertex_attrib_array(loc);
        gl.vertex_attrib_pointer_f32(loc, 2, glow::FLOAT, false, 8, 0);
        gl.bind_vertex_array(None);
        Ok(MaskGl {
            program,
            vao,
            _vbo: vbo,
            u_content: gl.get_uniform_location(program, "u_content"),
            u_mask: gl.get_uniform_location(program, "u_mask"),
            fbo: [gl.create_framebuffer()?, gl.create_framebuffer()?],
            tex: [gl.create_texture()?, gl.create_texture()?],
            size: [0, 0],
        })
    }

    /// Samakan ukuran kedua tekstur dengan layar. Framebuffer yang terikat berubah.
    unsafe fn resize(&mut self, gl: &glow::Context, size: [i32; 2]) {
        if self.size == size {
            return;
        }
        self.size = size;
        for (fbo, tex) in self.fbo.iter().zip(&self.tex) {
            gl.bind_texture(glow::TEXTURE_2D, Some(*tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                size[0],
                size[1],
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            for (name, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
            }
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(*fbo));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(*tex), 0);
        }
    }
}

pub type Shared = Arc<Mutex<Option<Program>>>;

impl Program {
    unsafe fn new(gl: &glow::Context) -> Result<Self, String> {
        let version = egui_glow::ShaderVersion::get(gl);
        let header = version.version_declaration();
        let (attr, vary_out, vary_in, tex, frag_decl, frag_out) = if version.is_new_shader_interface() {
            ("in", "out", "in", "texture", "out vec4 o_color;", "o_color")
        } else {
            ("attribute", "varying", "varying", "texture2D", "", "gl_FragColor")
        };
        let vs = format!(
            "{header}\n#ifdef GL_ES\nprecision mediump float;\n#endif\n\
             uniform vec2 u_screen;\n{attr} vec2 a_pos;\n{attr} vec2 a_tc;\n{attr} vec4 a_color;\n\
             {vary_out} vec2 v_tc;\n{vary_out} vec4 v_color;\n\
             void main() {{\n\
                 gl_Position = vec4(2.0 * a_pos.x / u_screen.x - 1.0, 1.0 - 2.0 * a_pos.y / u_screen.y, 0.0, 1.0);\n\
                 v_tc = a_tc;\n v_color = a_color;\n}}\n"
        );
        // egui mengunggah tekstur sebagai sRGB kalau didukung (syarat yang sama dengan egui_glow),
        // jadi hasil sampling-nya linear dan harus dikembalikan ke gamma seperti di shader egui.
        // Tanpa ini gambar dan gradasi tampak lebih gelap.
        let srgb = version == egui_glow::ShaderVersion::Es300 || gl.supported_extensions().iter().any(|e| e.contains("sRGB"));
        let decode = if srgb {
            "vec4 decode(vec4 c) {\n\
                 vec3 lo = c.rgb * 12.92;\n\
                 vec3 hi = 1.055 * pow(c.rgb, vec3(1.0 / 2.4)) - 0.055;\n\
                 return vec4(mix(hi, lo, vec3(lessThan(c.rgb, vec3(0.0031308)))), c.a);\n}"
        } else {
            "vec4 decode(vec4 c) { return c; }"
        };
        // u_white: komposit di atas putih dulu (untuk Darken), supaya area transparan tidak menggelapkan.
        let fs = format!(
            "{header}\n#ifdef GL_ES\nprecision mediump float;\n#endif\n\
             uniform sampler2D u_sampler;\nuniform float u_white;\n\
             {vary_in} vec2 v_tc;\n{vary_in} vec4 v_color;\n{frag_decl}\n{decode}\n\
             void main() {{\n\
                 vec4 c = v_color * decode({tex}(u_sampler, v_tc));\n\
                 c.rgb += u_white * (1.0 - c.a);\n\
                 {frag_out} = c;\n}}\n"
        );
        let program = compile(gl, &vs, &fs)?;
        let vao = gl.create_vertex_array()?;
        let vbo = gl.create_buffer()?;
        let ebo = gl.create_buffer()?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
        // Sama dengan `egui::epaint::Vertex`: pos (2×f32), uv (2×f32), warna (4×u8) = 20 byte.
        let stride = std::mem::size_of::<egui::epaint::Vertex>() as i32;
        for (name, size, ty, normalized, offset) in
            [("a_pos", 2, glow::FLOAT, false, 0), ("a_tc", 2, glow::FLOAT, false, 8), ("a_color", 4, glow::UNSIGNED_BYTE, true, 16)]
        {
            let loc = gl.get_attrib_location(program, name).ok_or_else(|| format!("missing attribute {name}"))?;
            gl.enable_vertex_attrib_array(loc);
            gl.vertex_attrib_pointer_f32(loc, size, ty, normalized, stride, offset);
        }
        gl.bind_vertex_array(None);
        Ok(Program {
            program,
            vao,
            vbo,
            ebo,
            u_screen: gl.get_uniform_location(program, "u_screen"),
            u_sampler: gl.get_uniform_location(program, "u_sampler"),
            u_white: gl.get_uniform_location(program, "u_white"),
            mask: None,
        })
    }
}

fn bytes<T>(slice: &[T]) -> &[u8] {
    // Aman: T di sini hanya `Vertex` (repr(C), tanpa padding) dan `u32`.
    unsafe { std::slice::from_raw_parts(slice.as_ptr().cast::<u8>(), std::mem::size_of_val(slice)) }
}

unsafe fn draw(painter: &egui_glow::Painter, info: &egui::PaintCallbackInfo, shared: &Shared, meshes: &[egui::Mesh], mode: u8) {
    let gl = painter.gl();
    let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        match Program::new(gl) {
            Ok(p) => *guard = Some(p),
            Err(e) => {
                eprintln!("blend shader: {e}");
                return;
            }
        }
    }
    let Some(p) = guard.as_ref() else { return };
    let [w, h] = info.screen_size_px;
    // egui mengatur viewport ke kotak callback; kita menggambar dengan koordinat layar penuh.
    gl.viewport(0, 0, w as i32, h as i32);
    gl.use_program(Some(p.program));
    gl.uniform_2_f32(p.u_screen.as_ref(), w as f32 / info.pixels_per_point, h as f32 / info.pixels_per_point);
    gl.uniform_1_i32(p.u_sampler.as_ref(), 0);
    gl.active_texture(glow::TEXTURE0);
    gl.enable(glow::BLEND);
    // Sumber (S) premultiplied, tujuan (D). Hasil:
    let (equation, src, dst, white) = match mode {
        1 => (glow::MIN, glow::ONE, glow::ONE, 1.0),                                    // min(S di atas putih, D)
        2 => (glow::FUNC_ADD, glow::DST_COLOR, glow::ONE_MINUS_SRC_ALPHA, 0.0),         // S·D + D·(1−a)
        3 => (glow::MAX, glow::ONE, glow::ONE, 0.0),                                    // max(S, D)
        4 => (glow::FUNC_ADD, glow::ONE, glow::ONE_MINUS_SRC_COLOR, 0.0),               // S + D·(1−S)
        5 => (glow::FUNC_ADD, glow::ONE, glow::ONE, 0.0),                               // S + D
        _ => (glow::FUNC_ADD, glow::ONE_MINUS_DST_COLOR, glow::ONE_MINUS_SRC_COLOR, 0.0), // S·(1−D) + D·(1−S)
    };
    gl.blend_equation_separate(equation, glow::FUNC_ADD);
    gl.blend_func_separate(src, dst, glow::ONE_MINUS_DST_ALPHA, glow::ONE);
    gl.uniform_1_f32(p.u_white.as_ref(), white);
    gl.bind_vertex_array(Some(p.vao));
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(p.vbo));
    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(p.ebo));
    for mesh in meshes {
        let Some(texture) = painter.texture(mesh.texture_id) else { continue };
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes(&mesh.vertices), glow::STREAM_DRAW);
        gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, bytes(&mesh.indices), glow::STREAM_DRAW);
        gl.draw_elements(glow::TRIANGLES, mesh.indices.len() as i32, glow::UNSIGNED_INT, 0);
    }
    gl.bind_vertex_array(None);
    // egui memulihkan state-nya sendiri setelah callback; persamaan blend dikembalikan untuk jaga-jaga.
    gl.blend_equation(glow::FUNC_ADD);
}

unsafe fn draw_masked(painter: &egui_glow::Painter, info: &egui::PaintCallbackInfo, shared: &Shared, mask: &[egui::Mesh], content: &[egui::Mesh]) {
    let gl = painter.gl();
    let mut guard = shared.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        match Program::new(gl) {
            Ok(p) => *guard = Some(p),
            Err(e) => {
                eprintln!("blend shader: {e}");
                return;
            }
        }
    }
    let Some(p) = guard.as_mut() else { return };
    if p.mask.is_none() {
        match MaskGl::new(gl) {
            Ok(m) => p.mask = Some(m),
            Err(e) => {
                eprintln!("mask shader: {e}");
                return;
            }
        }
    }
    let [w, h] = info.screen_size_px;
    let (w, h) = (w as i32, h as i32);
    // Framebuffer tujuan egui (layar, atau perantara) dikembalikan setelah menggambar ke tekstur.
    let target = std::num::NonZeroU32::new(gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) as u32).map(glow::NativeFramebuffer);
    let Some(m) = p.mask.as_mut() else { return };
    m.resize(gl, [w, h]);
    let Some(m) = p.mask.as_ref() else { return };

    // Tahap 1: alas dan isi ke teksturnya masing-masing. Scissor egui (kotak klip) tetap berlaku,
    // jadi hanya bagian yang terlihat yang dibersihkan dan digambar.
    gl.viewport(0, 0, w, h);
    gl.use_program(Some(p.program));
    gl.uniform_2_f32(p.u_screen.as_ref(), w as f32 / info.pixels_per_point, h as f32 / info.pixels_per_point);
    gl.uniform_1_i32(p.u_sampler.as_ref(), 0);
    gl.uniform_1_f32(p.u_white.as_ref(), 0.0);
    gl.active_texture(glow::TEXTURE0);
    gl.enable(glow::BLEND);
    gl.blend_equation(glow::FUNC_ADD);
    gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
    gl.bind_vertex_array(Some(p.vao));
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(p.vbo));
    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(p.ebo));
    for (fbo, meshes) in [(m.fbo[0], mask), (m.fbo[1], content)] {
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.clear_color(0.0, 0.0, 0.0, 0.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        for mesh in meshes {
            let Some(texture) = painter.texture(mesh.texture_id) else { continue };
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes(&mesh.vertices), glow::STREAM_DRAW);
            gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, bytes(&mesh.indices), glow::STREAM_DRAW);
            gl.draw_elements(glow::TRIANGLES, mesh.indices.len() as i32, glow::UNSIGNED_INT, 0);
        }
    }

    // Tahap 2: isi × alpha alas, ke tujuan semula.
    gl.bind_framebuffer(glow::FRAMEBUFFER, target);
    gl.use_program(Some(m.program));
    gl.uniform_1_i32(m.u_content.as_ref(), 0);
    gl.uniform_1_i32(m.u_mask.as_ref(), 1);
    gl.active_texture(glow::TEXTURE1);
    gl.bind_texture(glow::TEXTURE_2D, Some(m.tex[0]));
    gl.active_texture(glow::TEXTURE0);
    gl.bind_texture(glow::TEXTURE_2D, Some(m.tex[1]));
    gl.blend_func_separate(glow::ONE, glow::ONE_MINUS_SRC_ALPHA, glow::ONE_MINUS_DST_ALPHA, glow::ONE);
    gl.bind_vertex_array(Some(m.vao));
    gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
    gl.bind_vertex_array(None);
}

/// Bentuk callback clipping mask: `content` digambar hanya di dalam alpha `mask`.
pub fn masked(shared: Shared, clip: Rect, mask: Vec<egui::Mesh>, content: Vec<egui::Mesh>) -> egui::Shape {
    let meshes = Arc::new((mask, content));
    let callback = egui_glow::CallbackFn::new(move |info, painter| unsafe {
        draw_masked(painter, &info, &shared, &meshes.0, &meshes.1);
    });
    egui::Shape::Callback(egui::PaintCallback { rect: clip, callback: Arc::new(callback) })
}

/// Bentuk callback yang menggambar `meshes` dengan blend mode `mode` (1..=6).
pub fn callback(shared: Shared, clip: Rect, meshes: Vec<egui::Mesh>, mode: u8) -> egui::Shape {
    let meshes = Arc::new(meshes);
    let callback = egui_glow::CallbackFn::new(move |info, painter| unsafe {
        draw(painter, &info, &shared, &meshes, mode);
    });
    egui::Shape::Callback(egui::PaintCallback { rect: clip, callback: Arc::new(callback) })
}
