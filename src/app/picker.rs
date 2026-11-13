//! Color picker sendiri (kotak warna, hue, alpha, hex) plus editor gradasi dengan stop yang bisa digeser.

use crate::doc::{Gradient, Kind, Stop};
use eframe::egui::ecolor::Hsva;
use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind};

const WIDTH: f32 = 244.0;

pub struct Picker {
    /// (id node, slot) yang sedang diedit; ganti target = reset state.
    key: (u64, u8),
    hsva: Hsva,
    last: Option<[u8; 4]>,
    hex: String,
    /// Stop gradasi yang terpilih.
    pub(super) stop: usize,
    drag_stop: Option<usize>,
    /// Editor warna untuk stop terpilih sedang terbuka (panel di samping popup).
    stop_open: bool,
    side: Option<Rect>,
    /// Node yang popup fill-nya terbuka dengan gradasi linear: canvas menampilkan pegangan arahnya.
    pub(super) grad_open: Option<u64>,
    /// Posisi layar pegangan gradasi di canvas; klik di sana tidak menutup popup.
    pub(super) canvas_handles: Vec<Pos2>,
    /// Buka popup slot ini di frame berikutnya (dipakai tangkapan layar --bench).
    pub force_open: Option<u8>,
    /// Mode eyedropper sedang aktif.
    pub(super) eyedrop: Option<Eyedrop>,
    /// Warna hasil eyedropper yang menunggu diambil editor warna tujuannya: (tujuan, rgb, sisa frame).
    picked: Option<(Target, [u8; 3], u8)>,
}

/// Warna yang sedang diedit di popup: (kunci popup, stop gradasi).
type Target = ((u64, u8), Option<usize>);

/// Eyedropper: ambil warna dari piksel mana pun di jendela, lewat tangkapan layar yang dibekukan.
pub(super) struct Eyedrop {
    /// `None` = dipanggil dari toolbar: warna mengisi objek terpilih.
    target: Option<Target>,
    shot: Option<std::sync::Arc<egui::ColorImage>>,
    frames: u32,
}

impl Eyedrop {
    fn new(target: Option<Target>) -> Self {
        Eyedrop { target, shot: None, frames: 0 }
    }
}

impl Default for Picker {
    fn default() -> Self {
        Picker {
            key: (0, 0),
            hsva: Hsva::new(0.0, 0.0, 1.0, 1.0),
            last: None,
            hex: String::new(),
            stop: 0,
            drag_stop: None,
            stop_open: false,
            side: None,
            grad_open: None,
            canvas_handles: Vec::new(),
            force_open: None,
            eyedrop: None,
            picked: None,
        }
    }
}

fn c32(c: [u8; 4]) -> Color32 {
    super::col(c)
}

fn hex6(c: [u8; 4]) -> String {
    format!("{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    let full: String = match s.len() {
        3 => s.chars().flat_map(|c| [c, c]).collect(),
        6 => s.to_owned(),
        _ => return None,
    };
    let v = u32::from_str_radix(&full, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn checker(p: &egui::Painter, r: Rect, cell: f32) {
    p.rect_filled(r, CornerRadius::same(3), Color32::from_gray(200));
    let p = p.with_clip_rect(r.intersect(p.clip_rect()));
    let (cols, rows) = ((r.width() / cell).ceil() as i32, (r.height() / cell).ceil() as i32);
    for y in 0..rows {
        for x in 0..cols {
            if (x + y) % 2 == 0 {
                let min = pos2(r.left() + x as f32 * cell, r.top() + y as f32 * cell);
                p.rect_filled(Rect::from_min_size(min, vec2(cell, cell)), CornerRadius::ZERO, Color32::from_gray(140));
            }
        }
    }
}

/// Strip horizontal dengan warna per kolom (dipakai untuk hue, alpha, dan pratinjau gradasi).
fn strip(p: &egui::Painter, r: Rect, steps: usize, color: impl Fn(f32) -> Color32) {
    let mut mesh = egui::Mesh::default();
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = r.left() + r.width() * t;
        let c = color(t);
        mesh.colored_vertex(pos2(x, r.top()), c);
        mesh.colored_vertex(pos2(x, r.bottom()), c);
        if i > 0 {
            let b = (i as u32) * 2;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b - 1, b, b + 1);
        }
    }
    p.add(egui::Shape::mesh(mesh));
}

fn knob(p: &egui::Painter, c: Pos2, radius: f32, fill: Color32) {
    p.circle_filled(c, radius, fill);
    p.circle_stroke(c, radius, Stroke::new(2.0_f32, Color32::WHITE));
    p.circle_stroke(c, radius + 1.0, Stroke::new(1.0_f32, Color32::from_black_alpha(120)));
}

/// Slider horizontal 0..1; mengembalikan true kalau nilainya berubah.
fn slider(ui: &mut egui::Ui, height: f32, value: &mut f32, paint: impl FnOnce(&egui::Painter, Rect)) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    paint(ui.painter(), rect);
    let mut changed = false;
    if let Some(p) = resp.interact_pointer_pos() {
        let t = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        if t != *value {
            *value = t;
            changed = true;
        }
    }
    knob(ui.painter(), pos2(rect.left() + rect.width() * *value, rect.center().y), height * 0.5, Color32::TRANSPARENT);
    changed
}

/// Editor satu warna: kotak saturasi/kecerahan, hue, alpha, hex.
fn color_editor(ui: &mut egui::Ui, picker: &mut Picker, c: &mut [u8; 4], stop: Option<usize>) {
    let target: Target = (picker.key, stop);
    if picker.picked.is_some_and(|(t, ..)| t == target) {
        if let Some((_, [r, g, b], _)) = picker.picked.take() {
            // Warna yang tadinya transparan penuh dibuat terlihat.
            *c = [r, g, b, if c[3] == 0 { 255 } else { c[3] }];
        }
    }
    if picker.last != Some(*c) {
        // Warna berubah dari luar (atau target baru): sinkronkan, tapi pertahankan hue untuk abu-abu/hitam.
        let new = Hsva::from_srgba_unmultiplied(*c);
        let h = if new.s < 0.002 || new.v < 0.002 { picker.hsva.h } else { new.h };
        let s = if new.v < 0.002 { picker.hsva.s } else { new.s };
        picker.hsva = Hsva::new(h, s, new.v, new.a);
        picker.hex = hex6(*c);
        picker.last = Some(*c);
    }
    let mut hsva = picker.hsva;
    let mut changed = false;

    // Kotak saturasi (x) / kecerahan (y)
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 150.0), Sense::click_and_drag());
    {
        const N: usize = 12;
        let mut mesh = egui::Mesh::default();
        for yi in 0..=N {
            for xi in 0..=N {
                let (sx, sy) = (xi as f32 / N as f32, yi as f32 / N as f32);
                let color: Color32 = Hsva::new(hsva.h, sx, 1.0 - sy, 1.0).into();
                mesh.colored_vertex(pos2(rect.left() + rect.width() * sx, rect.top() + rect.height() * sy), color);
                if xi < N && yi < N {
                    let i = (yi * (N + 1) + xi) as u32;
                    let w = N as u32 + 1;
                    mesh.add_triangle(i, i + 1, i + w);
                    mesh.add_triangle(i + 1, i + w, i + w + 1);
                }
            }
        }
        ui.painter().with_clip_rect(rect).add(egui::Shape::mesh(mesh));
    }
    if let Some(p) = resp.interact_pointer_pos() {
        hsva.s = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        hsva.v = 1.0 - ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        changed = true;
    }
    let opaque: Color32 = Hsva::new(hsva.h, hsva.s, hsva.v, 1.0).into();
    knob(ui.painter(), pos2(rect.left() + rect.width() * hsva.s, rect.top() + rect.height() * (1.0 - hsva.v)), 7.0, opaque);

    ui.add_space(4.0);
    changed |= slider(ui, 14.0, &mut hsva.h, |p, r| {
        strip(p, r, 36, |t| Hsva::new(t, 1.0, 1.0, 1.0).into());
    });
    ui.add_space(2.0);
    changed |= slider(ui, 14.0, &mut hsva.a, |p, r| {
        checker(p, r, 7.0);
        let [red, green, blue, _] = opaque.to_array();
        strip(p, r, 2, |t| Color32::from_rgba_unmultiplied(red, green, blue, (t * 255.0) as u8));
    });

    if changed {
        picker.hsva = hsva;
        *c = hsva.to_srgba_unmultiplied();
        picker.last = Some(*c);
        picker.hex = hex6(*c);
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if icon_button(ui, "Pick a color from the screen", eyedropper_icon) {
            picker.eyedrop = Some(Eyedrop::new(Some(target)));
        }
        ui.weak("Hex");
        let resp = ui.add(egui::TextEdit::singleline(&mut picker.hex).desired_width(74.0).font(egui::TextStyle::Monospace));
        if resp.changed() {
            if let Some([r, g, b]) = parse_hex(&picker.hex) {
                *c = [r, g, b, c[3]];
                let new = Hsva::from_srgba_unmultiplied(*c);
                picker.hsva = Hsva::new(if new.s < 0.002 { picker.hsva.h } else { new.h }, new.s, new.v, new.a);
                picker.last = Some(*c);
            }
        }
        let mut pct = c[3] as f32 / 2.55;
        let dv = egui::DragValue::new(&mut pct).speed(1.0).range(0.0..=100.0).max_decimals(0).suffix("%");
        if ui.add_sized([56.0, 22.0], dv).on_hover_text("Opacity of this color").changed() {
            c[3] = (pct * 2.55).round().clamp(0.0, 255.0) as u8;
        }
    });
}

/// Ikon pipet: tabung miring dengan karet di kanan-atas.
pub(super) fn eyedropper_icon(p: &egui::Painter, c: Pos2, s: Stroke) {
    let at = |x: f32, y: f32| pos2(c.x + x, c.y + y);
    p.add(egui::Shape::closed_line(vec![at(-7.0, 7.0), at(-7.0, 4.5), at(0.5, -3.0), at(3.0, -0.5), at(-4.5, 7.0)], s));
    p.line_segment([at(-1.5, -5.0), at(5.0, 1.5)], s);
    p.circle_filled(at(4.5, -4.5), 3.2, s.color);
}

/// Tombol ikon kecil yang digambar sendiri (tidak bergantung pada glyph font).
fn icon_button(ui: &mut egui::Ui, tip: &str, draw: impl FnOnce(&egui::Painter, Pos2, Stroke)) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(4), Color32::from_white_alpha(22));
    }
    draw(ui.painter(), rect.center(), Stroke::new(1.3_f32, Color32::from_gray(210)));
    resp.on_hover_text(tip).clicked()
}

fn arrow_head(p: &egui::Painter, tip: Pos2, dir: f32, size: f32, stroke: Stroke) {
    for spread in [0.6_f32, -0.6] {
        p.line_segment([tip, tip + egui::Vec2::angled(dir + std::f32::consts::PI + spread) * size], stroke);
    }
}

/// Bar gradasi dengan stop yang bisa digeser; klik di area kosong menambah stop.
fn gradient_bar(ui: &mut egui::Ui, picker: &mut Picker, g: &mut Gradient) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::click_and_drag());
    let bar = Rect::from_min_max(pos2(rect.left() + 9.0, rect.top() + 22.0), pos2(rect.right() - 9.0, rect.bottom()));
    let sorted = g.sorted();
    checker(&ui.painter().with_clip_rect(bar), bar, 7.0);
    strip(&ui.painter().with_clip_rect(bar), bar, 48, |t| c32(Gradient::sample(&sorted, t)));

    let x_of = |pos: f32| bar.left() + bar.width() * pos.clamp(0.0, 1.0);
    let nearest = |x: f32, g: &Gradient| {
        g.stops
            .iter()
            .enumerate()
            .map(|(i, s)| (i, (x_of(s.pos) - x).abs()))
            .filter(|(_, d)| *d <= 10.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    if let Some(p) = resp.interact_pointer_pos() {
        let t = ((p.x - bar.left()) / bar.width()).clamp(0.0, 1.0);
        if resp.drag_started() {
            picker.drag_stop = nearest(p.x, g);
            if let Some(i) = picker.drag_stop {
                picker.stop = i;
            }
        } else if resp.dragged() {
            if let Some(s) = picker.drag_stop.and_then(|i| g.stops.get_mut(i)) {
                s.pos = (t * 100.0).round() / 100.0;
            }
        } else if resp.clicked() {
            // Klik stop: buka editor warnanya. Klik area kosong: stop baru.
            match nearest(p.x, g) {
                Some(i) => {
                    picker.stop_open = !(picker.stop_open && picker.stop == i);
                    picker.stop = i;
                }
                None => {
                    g.stops.push(Stop { pos: (t * 100.0).round() / 100.0, color: Gradient::sample(&sorted, t) });
                    picker.stop = g.stops.len() - 1;
                    picker.stop_open = true;
                }
            }
        }
    }
    if resp.drag_stopped() {
        picker.drag_stop = None;
    }
    // Penanda stop: kotak warna dengan ujung runcing ke bar; yang terpilih digambar terakhir (di atas).
    let mut order: Vec<usize> = (0..g.stops.len()).collect();
    order.sort_by_key(|i| *i == picker.stop);
    for i in order {
        let s = &g.stops[i];
        let selected = i == picker.stop;
        let c = pos2(x_of(s.pos), rect.top() + 9.0);
        let sq = Rect::from_center_size(c, vec2(12.0, 12.0));
        let border = if selected { super::ACCENT } else { Color32::from_gray(58) };
        let tip = vec![pos2(c.x - 4.5, sq.bottom() + 2.0), pos2(c.x + 4.5, sq.bottom() + 2.0), pos2(c.x, bar.top() + 1.0)];
        ui.painter().add(egui::Shape::convex_polygon(tip, border, Stroke::NONE));
        ui.painter().rect_filled(sq.expand(3.0), CornerRadius::same(5), border);
        ui.painter().rect_filled(sq, CornerRadius::same(2), c32([s.color[0], s.color[1], s.color[2], 255]));
    }
}

fn stops_list(ui: &mut egui::Ui, picker: &mut Picker, g: &mut Gradient) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Stops").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let add = icon_button(ui, "Add stop", |p, c, s| {
                p.line_segment([c - vec2(5.0, 0.0), c + vec2(5.0, 0.0)], s);
                p.line_segment([c - vec2(0.0, 5.0), c + vec2(0.0, 5.0)], s);
            });
            if add {
                // Stop baru di tengah celah terlebar.
                let sorted = g.sorted();
                let gap = sorted.windows(2).max_by(|a, b| (a[1].pos - a[0].pos).total_cmp(&(b[1].pos - b[0].pos)));
                let t = gap.map_or(0.5, |w| (w[0].pos + w[1].pos) * 0.5);
                g.stops.push(Stop { pos: (t * 100.0).round() / 100.0, color: Gradient::sample(&sorted, t) });
                picker.stop = g.stops.len() - 1;
            }
        });
    });
    let mut order: Vec<usize> = (0..g.stops.len()).collect();
    order.sort_by(|a, b| g.stops[*a].pos.total_cmp(&g.stops[*b].pos));
    let mut remove = None;
    let can_remove = g.stops.len() > 2;
    for i in order {
        let selected = i == picker.stop;
        let s = &mut g.stops[i];
        // Latar baris terpilih: slot shape dipesan dulu, diisi setelah ukuran baris diketahui.
        let bg = ui.painter().add(egui::Shape::Noop);
        let row = ui.horizontal(|ui| {
            ui.add_space(4.0);
            let mut pct = s.pos * 100.0;
            let dv = egui::DragValue::new(&mut pct).speed(0.5).range(0.0..=100.0).max_decimals(0).suffix("%");
            if ui.add_sized([46.0, 22.0], dv).on_hover_text("Position").changed() {
                s.pos = pct / 100.0;
            }
            let (sw, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
            checker(&ui.painter().with_clip_rect(sw), sw, 6.0);
            ui.painter().rect_filled(sw, CornerRadius::same(3), c32(s.color));
            let label = ui.add(egui::Label::new(egui::RichText::new(hex6(s.color)).monospace()).sense(Sense::click()));
            if resp.on_hover_text("Edit color").clicked() || label.clicked() {
                picker.stop_open = !(picker.stop_open && selected);
                picker.stop = i;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(2.0);
                let minus = ui.add_enabled_ui(can_remove, |ui| {
                    icon_button(ui, "Remove stop", |p, c, s| {
                        p.line_segment([c - vec2(5.0, 0.0), c + vec2(5.0, 0.0)], s);
                    })
                });
                if minus.inner {
                    remove = Some(i);
                }
                let mut alpha = s.color[3] as f32 / 2.55;
                let dv = egui::DragValue::new(&mut alpha).speed(1.0).range(0.0..=100.0).max_decimals(0).suffix("%");
                if ui.add_sized([46.0, 22.0], dv).on_hover_text("Opacity").changed() {
                    s.color[3] = (alpha * 2.55).round().clamp(0.0, 255.0) as u8;
                }
            });
        });
        if selected {
            let r = row.response.rect.expand2(vec2(0.0, 2.0));
            ui.painter().set(bg, egui::Shape::rect_filled(r, CornerRadius::same(4), super::ACCENT.gamma_multiply(0.22)));
        }
    }
    if let Some(i) = remove {
        g.stops.remove(i);
        if picker.stop > i || picker.stop >= g.stops.len() {
            picker.stop = picker.stop.saturating_sub(1);
        }
    }
}

fn popup(ui: &mut egui::Ui, picker: &mut Picker, key: (u64, u8), c: &mut [u8; 4], gradient: Option<&mut Option<Gradient>>) {
    ui.set_min_width(WIDTH);
    ui.set_max_width(WIDTH);
    if picker.key != key {
        picker.key = key;
        picker.stop = 0;
        picker.last = None;
        picker.drag_stop = None;
        picker.stop_open = false;
    }
    picker.side = None;
    if let Some(slot) = gradient {
        let mode = match slot {
            None => 0,
            Some(g) if !g.radial => 1,
            Some(_) => 2,
        };
        ui.horizontal(|ui| {
            for (i, label) in ["Solid", "Linear", "Radial"].into_iter().enumerate() {
                if ui.selectable_label(mode == i, label).clicked() && mode != i {
                    if i == 0 {
                        *slot = None;
                    } else {
                        let mut g = slot.take().unwrap_or_else(|| Gradient::from_color(*c));
                        g.radial = i == 2;
                        *slot = Some(g);
                    }
                }
            }
            let Some(g) = slot.as_mut() else { return };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !g.radial {
                    let rotate = icon_button(ui, "Rotate 90°", |p, c, s| {
                        let at = |deg: f32| c + egui::Vec2::angled(deg.to_radians()) * 5.5;
                        let arc: Vec<Pos2> = (0..=12).map(|i| at(-60.0 + i as f32 * 25.0)).collect();
                        p.add(egui::Shape::line(arc, s));
                        arrow_head(p, at(240.0), 330.0_f32.to_radians(), 4.0, s);
                    });
                    if rotate {
                        // Putar garis gradasi 90° terhadap pusat kotak.
                        let [x1, y1, x2, y2] = g.line();
                        g.line = Some([1.0 - y1, x1, 1.0 - y2, x2]);
                    }
                }
                let flip = icon_button(ui, "Reverse gradient", |p, c, s| {
                    let (a, b) = (c + vec2(-6.0, -3.0), c + vec2(6.0, -3.0));
                    p.line_segment([a, b], s);
                    arrow_head(p, b, 0.0, 3.5, s);
                    let (a, b) = (c + vec2(6.0, 3.0), c + vec2(-6.0, 3.0));
                    p.line_segment([a, b], s);
                    arrow_head(p, b, std::f32::consts::PI, 3.5, s);
                });
                if flip {
                    for s in &mut g.stops {
                        s.pos = 1.0 - s.pos;
                    }
                }
            });
        });
        ui.add_space(2.0);
        if let Some(g) = slot.as_mut() {
            gradient_bar(ui, picker, g);
            picker.stop = picker.stop.min(g.stops.len().saturating_sub(1));
            ui.add_space(6.0);
            stops_list(ui, picker, g);
            picker.stop = picker.stop.min(g.stops.len().saturating_sub(1));
            // Editor warna stop terpilih: panel terpisah di kiri popup, hanya saat stop diklik.
            if picker.stop_open {
                let index = picker.stop;
                if let Some(stop) = g.stops.get_mut(index) {
                    let at = pos2(ui.max_rect().left() - WIDTH - 28.0, ui.max_rect().top() - 7.0);
                    let area = egui::Area::new(ui.id().with("stop_color")).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_min_width(WIDTH);
                            ui.set_max_width(WIDTH);
                            color_editor(ui, picker, &mut stop.color, Some(index));
                        });
                    });
                    picker.side = Some(area.response.rect);
                }
            }
            return;
        }
    }
    color_editor(ui, picker, c, None);
}

fn swatch(ui: &egui::Ui, rect: Rect, c: [u8; 4], gradient: Option<&Gradient>) {
    checker(ui.painter(), rect, 6.0);
    match gradient {
        Some(g) => {
            let sorted = g.sorted();
            strip(&ui.painter().with_clip_rect(rect), rect, 16, |t| c32(Gradient::sample(&sorted, t)));
        }
        None => {
            ui.painter().rect_filled(rect, CornerRadius::same(3), c32(c));
        }
    }
    ui.painter().rect_stroke(rect, CornerRadius::same(3), Stroke::new(1.0_f32, Color32::from_gray(96)), StrokeKind::Inside);
}

/// Baris "paint" di panel: contoh warna (klik untuk membuka picker), keterangan, tombol tambah/hapus.
/// `gradient = None` berarti hanya warna solid (stroke, teks).
pub fn paint_row(
    ui: &mut egui::Ui,
    picker: &mut Picker,
    key: (u64, u8),
    fill: &mut Option<[u8; 4]>,
    mut gradient: Option<&mut Option<Gradient>>,
    default: [u8; 4],
) {
    let mut remove = false;
    let has_gradient = gradient.is_some();
    if has_gradient {
        picker.grad_open = None;
    }
    ui.horizontal(|ui| {
        let Some(c) = fill.as_mut() else {
            ui.weak("None");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+").on_hover_text("Add").clicked() {
                    *fill = Some(default);
                }
            });
            return;
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 22.0), Sense::click());
        let current = gradient.as_deref().and_then(|g| g.as_ref());
        swatch(ui, rect, *c, current);
        let label = match current {
            Some(g) if g.radial => "Radial".to_owned(),
            Some(_) => "Linear".to_owned(),
            None => format!("{}   {:.0}%", hex6(*c), c[3] as f32 / 2.55),
        };
        let text = ui.add(egui::Label::new(label).sense(Sense::click()));
        let popup_id = ui.make_persistent_id(("paint_popup", key.1));
        if resp.on_hover_text("Edit color").clicked() || text.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        if picker.force_open == Some(key.1) {
            picker.force_open = None;
            ui.memory_mut(|m| m.open_popup(popup_id));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("−").on_hover_text("Remove").clicked() {
                remove = true;
            }
        });
        // Jangkar popup di kiri baris supaya muat di panel.
        let anchor = ui.interact(rect, popup_id.with("anchor"), Sense::hover());
        // Popup tidak menutup sendiri saat klik di luar: pegangan gradasi di canvas dan panel warna
        // stop ada di luar popup. Penutupan diurus di bawah.
        egui::popup::popup_below_widget(ui, popup_id, &anchor, egui::PopupCloseBehavior::IgnoreClicks, |ui| {
            popup(ui, picker, key, c, gradient.as_deref_mut());
        });
        if ui.memory(|m| m.is_popup_open(popup_id)) {
            // Pegangan gradasi di canvas tampil untuk linear maupun radial.
            if gradient.as_deref().is_some_and(|g| g.is_some()) {
                picker.grad_open = Some(key.0);
            }
            let press = ui.input(|i| if i.pointer.any_pressed() { i.pointer.interact_pos() } else { None });
            // Saat eyedropper aktif, klik di mana pun adalah pengambilan warna, bukan "klik di luar".
            if let Some(p) = press.filter(|_| picker.eyedrop.is_none()) {
                let inside = ui.ctx().memory(|m| m.area_rect(popup_id)).is_some_and(|r| r.contains(p))
                    || picker.side.is_some_and(|r| r.contains(p))
                    || rect.contains(p)
                    || text.rect.contains(p);
                let on_handle = has_gradient && picker.canvas_handles.iter().any(|h| h.distance(p) <= 12.0);
                if !inside && !on_handle {
                    ui.memory_mut(|m| m.close_popup());
                    if has_gradient {
                        picker.grad_open = None;
                    }
                }
            }
        }
    });
    if remove {
        *fill = None;
        if let Some(g) = gradient {
            *g = None;
        }
    }
}

// ---------------------------------------------------------------- eyedropper

/// Warna piksel tangkapan layar di posisi layar `pos` (titik logis), digeser `dx`/`dy` piksel fisik.
fn sample(img: &egui::ColorImage, ppp: f32, pos: Pos2, dx: i32, dy: i32) -> [u8; 3] {
    let (w, h) = (img.size[0] as i32, img.size[1] as i32);
    if w == 0 || h == 0 {
        return [0, 0, 0];
    }
    let x = ((pos.x * ppp).floor() as i32 + dx).clamp(0, w - 1);
    let y = ((pos.y * ppp).floor() as i32 + dy).clamp(0, h - 1);
    let c = img.pixels[(y * w + x) as usize];
    [c.r(), c.g(), c.b()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eyedropper_samples_physical_pixels() {
        // 4×2 piksel fisik pada skala 2×: titik logis (1.2, 0.6) jatuh di piksel (2, 1).
        let mut img = egui::ColorImage::new([4, 2], Color32::BLACK);
        img.pixels[4 + 2] = Color32::from_rgb(10, 20, 30);
        img.pixels[4 + 3] = Color32::from_rgb(40, 50, 60);
        assert_eq!(sample(&img, 2.0, pos2(1.2, 0.6), 0, 0), [10, 20, 30]);
        assert_eq!(sample(&img, 2.0, pos2(1.2, 0.6), 1, 0), [40, 50, 60]);
        // Di luar gambar: dijepit ke tepi, tidak panik.
        assert_eq!(sample(&img, 2.0, pos2(1.2, 0.6), 9, 9), [40, 50, 60]);
        assert_eq!(sample(&img, 2.0, pos2(-5.0, -5.0), 0, 0), [0, 0, 0]);
    }
}

impl super::App {
    /// Tombol toolbar / tombol I: warna yang diambil mengisi objek terpilih.
    pub(super) fn toggle_eyedropper(&mut self) {
        let picker = &mut self.ext.picker;
        picker.eyedrop = if picker.eyedrop.is_some() { None } else { Some(Eyedrop::new(None)) };
    }

    /// Dijalankan tiap frame setelah seluruh UI digambar: lapisan penangkap klik, kaca pembesar,
    /// dan penerapan warna yang diambil.
    pub(super) fn eyedropper(&mut self, ctx: &egui::Context) {
        let picker = &mut self.ext.picker;
        if let Some((.., ttl)) = &mut picker.picked {
            // Editor warna tujuan biasanya mengambilnya di frame berikutnya; kalau popup-nya sudah
            // tertutup, jangan sampai warna ini muncul belakangan.
            match ttl.checked_sub(1) {
                Some(left) => *ttl = left,
                None => picker.picked = None,
            }
        }
        let Some(eye) = picker.eyedrop.as_mut() else { return };
        eye.frames += 1;
        let screen = ctx.screen_rect();
        let ppp = ctx.pixels_per_point();

        // Lapisan di atas segalanya: menelan klik/scroll supaya canvas dan panel tidak ikut bereaksi,
        // dan tampilan tetap sama dengan tangkapan layar yang dibekukan.
        let resp = egui::Area::new(egui::Id::new("kanvas_eyedropper"))
            .order(egui::Order::Tooltip)
            .fixed_pos(screen.min)
            .show(ctx, |ui| ui.allocate_exact_size(screen.size(), Sense::click_and_drag()).1)
            .inner;
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        if resp.secondary_clicked() {
            picker.eyedrop = None;
            return;
        }

        // Jendela berubah ukuran: tangkapan lama tidak cocok lagi.
        let stale = eye.shot.as_ref().is_some_and(|img| {
            (img.size[0] as f32 - screen.width() * ppp).abs() > 2.0 || (img.size[1] as f32 - screen.height() * ppp).abs() > 2.0
        });
        if stale {
            *eye = Eyedrop::new(eye.target);
        }
        if eye.shot.is_none() {
            // Tunggu beberapa frame dulu supaya tooltip/hover tombol hilang dari tangkapan.
            if eye.frames % 60 == 3 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            } else if eye.frames > 3 {
                eye.shot = ctx.input(|i| {
                    i.events.iter().find_map(|e| match e {
                        egui::Event::Screenshot { image, .. } => Some(image.clone()),
                        _ => None,
                    })
                });
            }
            ctx.request_repaint();
        }
        let (Some(img), Some(pos)) = (eye.shot.clone(), ctx.input(|i| i.pointer.latest_pos())) else { return };
        let rgb = sample(&img, ppp, pos, 0, 0);
        let hex = hex6([rgb[0], rgb[1], rgb[2], 255]);

        // Kaca pembesar: piksel di sekitar kursor, dengan piksel yang akan diambil di tengah.
        const N: i32 = 11;
        const CELL: f32 = 10.0;
        const LABEL: f32 = 24.0;
        let size = N as f32 * CELL;
        let mut min = pos + vec2(20.0, 20.0);
        if min.x + size > screen.right() - 6.0 {
            min.x = pos.x - 20.0 - size;
        }
        if min.y + size + LABEL > screen.bottom() - 6.0 {
            min.y = pos.y - 20.0 - size - LABEL;
        }
        let grid = Rect::from_min_size(min, vec2(size, size));
        let frame = Rect::from_min_max(grid.min, grid.max + vec2(0.0, LABEL)).expand(3.0);
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Debug, egui::Id::new("kanvas_eyedropper_loupe")));
        p.rect_filled(frame, CornerRadius::same(6), Color32::from_gray(28));
        let cells = p.with_clip_rect(grid);
        for gy in 0..N {
            for gx in 0..N {
                let [r, g, b] = sample(&img, ppp, pos, gx - N / 2, gy - N / 2);
                let cell = Rect::from_min_size(grid.min + vec2(gx as f32 * CELL, gy as f32 * CELL), vec2(CELL + 0.5, CELL + 0.5));
                cells.rect_filled(cell, CornerRadius::ZERO, Color32::from_rgb(r, g, b));
            }
        }
        let center = Rect::from_center_size(grid.center(), vec2(CELL, CELL));
        p.rect_stroke(center.expand(1.0), CornerRadius::ZERO, Stroke::new(1.0_f32, Color32::BLACK), StrokeKind::Outside);
        p.rect_stroke(center, CornerRadius::ZERO, Stroke::new(1.0_f32, Color32::WHITE), StrokeKind::Outside);
        p.rect_stroke(frame, CornerRadius::same(6), Stroke::new(1.0_f32, Color32::from_gray(96)), StrokeKind::Inside);
        let chip = Rect::from_min_size(pos2(grid.left() + 2.0, grid.bottom() + 6.0), vec2(14.0, 14.0));
        p.rect_filled(chip, CornerRadius::same(3), Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
        p.rect_stroke(chip, CornerRadius::same(3), Stroke::new(1.0_f32, Color32::from_gray(96)), StrokeKind::Inside);
        p.text(
            pos2(chip.right() + 6.0, chip.center().y),
            egui::Align2::LEFT_CENTER,
            format!("#{hex}"),
            egui::FontId::monospace(12.0),
            Color32::WHITE,
        );

        if !(resp.clicked() || resp.drag_stopped()) {
            return;
        }
        let target = eye.target;
        picker.eyedrop = None;
        ctx.request_repaint();
        let color = [rgb[0], rgb[1], rgb[2], 255];
        match target {
            Some(t) => {
                picker.picked = Some((t, rgb, 3));
                self.status = format!("Picked #{hex}.");
            }
            None if self.sel.is_empty() => {
                ctx.copy_text(format!("#{hex}"));
                self.status = format!("Picked #{hex} and copied it. Select an object first to fill it with the picked color.");
            }
            None => {
                self.edit_selected(|n| match n.kind {
                    // Fill pada gambar berarti mewarnai ulang seluruh gambar; jangan lakukan diam-diam.
                    Kind::Image => {}
                    Kind::Line => n.stroke = Some(color),
                    _ => {
                        n.fill = Some(color);
                        n.gradient = None;
                        for span in &mut n.spans {
                            span.style.fill = color;
                        }
                    }
                });
                self.status = format!("Picked #{hex} for the selection.");
            }
        }
    }
}
