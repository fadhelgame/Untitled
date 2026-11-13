//! Preset ukuran frame: tampil di panel kanan saat Frame tool aktif.

use super::*;

/// (kategori, [(nama, lebar, tinggi)]).
const PRESETS: &[(&str, &[(&str, f32, f32)])] = &[
    (
        "Phone",
        &[
            ("iPhone 17", 402.0, 874.0),
            ("iPhone 17 Pro Max", 440.0, 956.0),
            ("iPhone Air", 420.0, 912.0),
            ("iPhone 16", 393.0, 852.0),
            ("iPhone 16 Plus", 430.0, 932.0),
            ("iPhone 13 & 14", 390.0, 844.0),
            ("iPhone 14 Plus", 428.0, 926.0),
            ("iPhone 13 mini", 375.0, 812.0),
            ("iPhone SE", 320.0, 568.0),
            ("iPhone 8", 375.0, 667.0),
            ("iPhone 8 Plus", 414.0, 736.0),
            ("Android Compact", 412.0, 917.0),
            ("Android Medium", 700.0, 840.0),
        ],
    ),
    (
        "Tablet",
        &[
            ("iPad mini 8.3", 744.0, 1133.0),
            ("iPad Pro 11\"", 834.0, 1194.0),
            ("iPad Pro 12.9\"", 1024.0, 1366.0),
            ("Surface Pro 8", 1440.0, 960.0),
            ("Android Expanded", 1280.0, 800.0),
        ],
    ),
    (
        "Desktop",
        &[
            ("MacBook Air", 1280.0, 832.0),
            ("MacBook Pro 14\"", 1512.0, 982.0),
            ("MacBook Pro 16\"", 1728.0, 1117.0),
            ("Desktop", 1440.0, 1024.0),
            ("Desktop HD", 1920.0, 1080.0),
            ("Wireframe", 1440.0, 1024.0),
            ("TV", 1280.0, 720.0),
        ],
    ),
    (
        "Presentation",
        &[
            ("Slide 16:9", 1920.0, 1080.0),
            ("Slide 4:3", 1024.0, 768.0),
            ("Slide 16:10", 1920.0, 1200.0),
        ],
    ),
    (
        "Watch",
        &[
            ("Apple Watch 46mm", 208.0, 248.0),
            ("Apple Watch 45mm", 198.0, 242.0),
            ("Apple Watch 44mm", 184.0, 224.0),
            ("Apple Watch 42mm", 187.0, 223.0),
            ("Apple Watch 41mm", 176.0, 215.0),
            ("Apple Watch 40mm", 162.0, 197.0),
        ],
    ),
    (
        "Paper",
        &[
            ("A3", 842.0, 1191.0),
            ("A4", 595.0, 842.0),
            ("A5", 420.0, 595.0),
            ("A6", 297.0, 420.0),
            ("Letter", 612.0, 792.0),
            ("Legal", 612.0, 1008.0),
            ("Tabloid", 792.0, 1224.0),
            ("Business card", 252.0, 144.0),
            ("Poster 18×24 in", 1296.0, 1728.0),
        ],
    ),
    (
        "Social media",
        &[
            ("Instagram post", 1080.0, 1350.0),
            ("Instagram square", 1080.0, 1080.0),
            ("Instagram story", 1080.0, 1920.0),
            ("TikTok video", 1080.0, 1920.0),
            ("X (Twitter) post", 1200.0, 675.0),
            ("X (Twitter) header", 1500.0, 500.0),
            ("Facebook post", 1200.0, 630.0),
            ("Facebook cover", 820.0, 312.0),
            ("YouTube thumbnail", 1280.0, 720.0),
            ("YouTube banner", 2560.0, 1440.0),
            ("LinkedIn post", 1200.0, 627.0),
            ("LinkedIn cover", 1584.0, 396.0),
            ("Pinterest pin", 1000.0, 1500.0),
            ("Dribbble shot", 400.0, 300.0),
            ("Dribbble shot HD", 800.0, 600.0),
        ],
    ),
];

impl App {
    /// Isi panel kanan selama Frame tool aktif: daftar preset per kategori.
    pub(super) fn frame_presets(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(theme::title("Frame"));
        ui.weak("Pick a preset, or drag on the canvas for a custom size.");
        ui.add_space(4.0);
        let mut picked = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (category, items) in PRESETS {
                ui.separator();
                egui::CollapsingHeader::new(egui::RichText::new(*category).strong().size(12.5))
                    .id_salt(("frame_presets", *category))
                    .show(ui, |ui| {
                        for item in *items {
                            if preset_row(ui, item) {
                                picked = Some(*item);
                            }
                        }
                    });
            }
            ui.add_space(12.0);
        });
        if let Some((name, w, h)) = picked {
            self.add_preset_frame(name, w, h);
        }
    }

    /// Buat frame seukuran preset di tengah layar; geser ke kanan bila tempatnya sudah terisi.
    fn add_preset_frame(&mut self, name: &str, w: f32, h: f32) {
        let o = self.canvas_rect.min;
        let c = self.view.to_world(o, self.canvas_rect.center());
        let (mut x, y) = ((c.x - w * 0.5).round(), (c.y - h * 0.5).round());
        for _ in 0..=self.doc.nodes.len() {
            let hit = self.doc.nodes.iter().filter(|n| n.visible).map(|n| n.bounds()).find(|b| b.0 < x + w && b.2 > x && b.1 < y + h && b.3 > y);
            match hit {
                Some(b) => x = (b.2 + 100.0).round(),
                None => break,
            }
        }
        let pre = self.doc.clone();
        let id = self.doc.alloc_id();
        let mut n = Node::new(id, Kind::Frame, x, y);
        let same = self.doc.nodes.iter().filter(|f| f.kind == Kind::Frame && f.name.starts_with(name)).count();
        n.name = format!("{name} - {}", same + 1);
        (n.w, n.h) = (w, h);
        n.fill = Some([255, 255, 255, 255]);
        self.doc.insert(n, None);
        self.push_undo(pre);
        self.sel = vec![id];
        self.tool = Tool::Select;
        // Frame yang tidak muat di layar: mundurkan zoom sampai terlihat utuh.
        let (tl, br) = (self.view.to_world(o, self.canvas_rect.min), self.view.to_world(o, self.canvas_rect.max));
        if x < tl.x || y < tl.y || x + w > br.x || y + h > br.y {
            self.zoom_to((x, y, x + w, y + h), self.view.zoom.min(1.0));
        }
    }
}

/// Satu baris preset: nama di kiri, ukuran di kanan. `true` saat diklik.
fn preset_row(ui: &mut egui::Ui, (name, w, h): &(&str, f32, f32)) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    if resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(8), ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let font = FontId::proportional(13.5);
    painter.text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, name, font.clone(), ui.visuals().text_color());
    painter.text(
        pos2(rect.right() - 10.0, rect.center().y),
        Align2::RIGHT_CENTER,
        format!("{w}×{h}"),
        font,
        ui.visuals().weak_text_color(),
    );
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}
