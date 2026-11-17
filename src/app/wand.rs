//! Magic wand: klik satu objek, semua objek dengan properti yang sama ikut terpilih.

use super::*;

/// Properti yang dibandingkan saat magic wand diklik.
pub(super) struct Wand {
    pub kind: bool,
    pub fill: bool,
    pub stroke: bool,
    pub stroke_w: bool,
    pub opacity: bool,
    pub font: bool,
    pub font_size: bool,
    /// Selisih maksimum per kanal warna (0 = harus persis sama).
    pub tolerance: u8,
    /// Hanya mencari di frame yang sama dengan objek yang diklik.
    pub same_frame: bool,
}

impl Default for Wand {
    fn default() -> Self {
        Wand {
            kind: false,
            fill: true,
            stroke: false,
            stroke_w: false,
            opacity: false,
            font: false,
            font_size: false,
            tolerance: 0,
            same_frame: false,
        }
    }
}

fn color_eq(a: Option<[u8; 4]>, b: Option<[u8; 4]>, tol: u8) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol),
        _ => false,
    }
}

impl Wand {
    fn any(&self) -> bool {
        self.kind || self.fill || self.stroke || self.stroke_w || self.opacity || self.font || self.font_size
    }

    /// `c` cocok dengan objek acuan `r` untuk semua properti yang dicentang.
    pub(super) fn matches(&self, r: &Node, c: &Node) -> bool {
        if self.kind && r.kind != c.kind {
            return false;
        }
        if self.fill {
            // Gradasi hanya cocok dengan gradasi yang identik.
            let (rg, cg) = (r.gradient.as_ref().filter(|_| r.fill.is_some()), c.gradient.as_ref().filter(|_| c.fill.is_some()));
            let same = match (rg, cg) {
                (None, None) => color_eq(r.fill, c.fill, self.tolerance),
                (a, b) => a == b,
            };
            if !same {
                return false;
            }
        }
        if self.stroke && !color_eq(r.stroke, c.stroke, self.tolerance) {
            return false;
        }
        if self.stroke_w && (r.stroke.is_some() != c.stroke.is_some() || (r.stroke.is_some() && (r.stroke_w - c.stroke_w).abs() > 0.01)) {
            return false;
        }
        if self.opacity && (r.opacity - c.opacity).abs() > 0.005 {
            return false;
        }
        // Properti huruf hanya berlaku kalau acuannya teks; yang cocok pun harus teks.
        if r.kind == Kind::Text && (self.font || self.font_size) {
            if c.kind != Kind::Text {
                return false;
            }
            if self.font && (r.font != c.font || r.weight != c.weight || r.italic != c.italic) {
                return false;
            }
            if self.font_size && (r.font_size - c.font_size).abs() > 0.01 {
                return false;
            }
        }
        true
    }
}

impl App {
    /// Id semua objek yang cocok dengan objek `id` menurut pengaturan wand (termasuk `id` sendiri).
    pub(super) fn similar_to(&self, id: u64) -> Vec<u64> {
        let Some(r) = self.doc.find(id) else { return Vec::new() };
        if !self.wand.any() {
            return vec![id];
        }
        let scope = if self.wand.same_frame { Some(self.doc.parent_of(id)) } else { None };
        let mut out = Vec::new();
        // Isi frame yang tersembunyi atau terkunci tidak ikut, di kedalaman mana pun.
        fn each<'a>(n: &'a Node, parent: Option<u64>, check: &mut impl FnMut(&'a Node, Option<u64>)) {
            if !n.visible || n.locked {
                return;
            }
            check(n, parent);
            for c in &n.children {
                each(c, Some(n.id), check);
            }
        }
        let mut check = |c: &Node, parent: Option<u64>| {
            // Frame hanya ikut kalau yang diklik juga frame.
            let frame_ok = c.kind != Kind::Frame || r.kind == Kind::Frame;
            if frame_ok && scope.map_or(true, |s| s == parent) && self.wand.matches(r, c) {
                out.push(c.id);
            }
        };
        for top in &self.doc.nodes {
            each(top, None, &mut check);
        }
        out
    }

    /// Klik dengan magic wand. Shift = tambah ke seleksi, Alt = kurangi dari seleksi.
    pub(super) fn wand_click(&mut self, p: Pos2, wp: Pos2, inp: &Inp) {
        let tol = 4.0 / self.view.zoom;
        let Some(id) = self.hit(p, wp, tol) else {
            if !inp.mods.shift && !inp.mods.alt {
                self.sel.clear();
            }
            return;
        };
        let found = self.similar_to(id);
        if inp.mods.alt {
            self.sel.retain(|s| !found.contains(s));
        } else if inp.mods.shift {
            for f in found {
                if !self.sel.contains(&f) {
                    self.sel.push(f);
                }
            }
        } else {
            self.sel = found;
        }
        self.status = format!("Magic wand: {} selected", self.sel.len());
    }

    /// Pengaturan magic wand di panel kanan (tampil selama tool-nya aktif).
    pub(super) fn wand_options(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(theme::title("Magic wand"));
        ui.weak("Click an object to select everything that matches it.");
        ui.add_space(2.0);
        let w = &mut self.wand;
        ui.columns(2, |cols| {
            cols[0].checkbox(&mut w.fill, "Fill color");
            cols[0].checkbox(&mut w.stroke, "Stroke color");
            cols[0].checkbox(&mut w.stroke_w, "Stroke weight");
            cols[0].checkbox(&mut w.opacity, "Opacity");
            cols[1].checkbox(&mut w.kind, "Object type");
            cols[1].checkbox(&mut w.font, "Font");
            cols[1].checkbox(&mut w.font_size, "Font size");
        });
        ui.horizontal(|ui| {
            ui.add_sized([110.0, 28.0], egui::DragValue::new(&mut w.tolerance).speed(0.5).range(0..=255).prefix("Tolerance  "))
                .on_hover_text("How far colors may differ (0 = exact match)");
            ui.checkbox(&mut w.same_frame, "Same frame only");
        });
        ui.label(egui::RichText::new("Shift+click adds · Alt+click removes").size(11.0).weak());
        ui.separator();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wand_matches_checked_properties_only() {
        let mut a = Node::new(1, Kind::Rect, 0.0, 0.0);
        a.fill = Some([255, 0, 0, 255]);
        let mut b = Node::new(2, Kind::Text, 0.0, 0.0);
        b.fill = Some([250, 0, 0, 255]);
        let mut w = Wand::default();
        assert!(!w.matches(&a, &b));
        w.tolerance = 5;
        assert!(w.matches(&a, &b));
        w.kind = true;
        assert!(!w.matches(&a, &b));
        // Acuan teks dengan kriteria font: objek bukan teks tidak cocok.
        w.kind = false;
        w.font = true;
        assert!(!w.matches(&b, &a));
        assert!(w.matches(&a, &b));
    }
}
