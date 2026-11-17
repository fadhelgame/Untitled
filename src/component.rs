//! Components. A frame flagged `component` is a main component; a frame with `instance_of` is an
//! instance of one. An instance holds real copies of the main's layers, each remembering in
//! `source` which node of the main it mirrors, so drawing, export and hit testing treat it as
//! any other frame.
//!
//! Nothing records what an instance overrides. When a main changes from `old` to `new`, a
//! property of an instance follows only if it still had the `old` value; anything else was
//! changed by the user and stays (see `Node::follow`). Position, size and structure are not
//! overridable: they are rebuilt from the main every time, laid out in the box of the instance
//! by the constraints that the layers have in the main, or by its auto layout (see `layout.rs`).

use crate::doc::{Doc, Kind, Node};
use crate::layout::{self, Rect, Sizing};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Mains can hold instances of other mains, so one change may take several rounds to settle.
/// The cap only matters if a cycle slipped past `Doc::would_cycle`.
const SYNC_ROUNDS: usize = 16;

/// Sorts every field of `Node` into what an instance may override (`follow`), what it may
/// override as one unit (`by_hand`) and what always comes from the main or belongs to the node
/// itself (`fixed`). A field missing from the lists does not compile.
macro_rules! node_fields {
    (follow: [$($f:ident),* $(,)?], by_hand: [$($h:ident),* $(,)?], fixed: [$($x:ident),* $(,)?] $(,)?) => {
        impl Node {
            /// The main went from `old` to `new`: take over each property that still had the
            /// old value. The text and its styled ranges go together, since the ranges are
            /// character positions in that text.
            pub(crate) fn follow(&mut self, old: &Node, new: &Node) {
                let Node { $($f: _,)* $($h: _,)* $($x: _,)* } = new;
                $(if self.$f == old.$f {
                    self.$f = new.$f.clone();
                })*
                if self.text == old.text && self.spans == old.spans {
                    self.text = new.text.clone();
                    self.spans = new.spans.clone();
                }
            }

            /// Drop every override: all overridable properties as in `main`.
            fn adopt(&mut self, main: &Node) {
                $(self.$f = main.$f.clone();)*
                $(self.$h = main.$h.clone();)*
            }
        }
    };
}

node_fields! {
    follow: [
        fill, gradient, stroke, stroke_w, radius, font, weight, italic, font_size, line_height, letter_spacing, align,
        underline, strike, list, valign,
        image, crop, flip_h, flip_v, blend, opacity, visible, locked, tag, dash, gap, stroke_align, cap, join, shadow, blur,
        fill_style, stroke_style, type_style,
    ],
    by_hand: [text, spans],
    fixed: [id, name, kind, x, y, w, h, rotation, wrap, fixed_h, points, closed, holes, group, clip, children, component, instance_of, source, constrain_h, constrain_v, layout, size_h, size_v, absolute],
}

impl Node {
    /// An instance of its own: placed by the user, not a part of another instance.
    pub fn is_instance(&self) -> bool {
        self.instance_of.is_some() && self.source.is_none()
    }

    /// A main component or an instance (drawn in the component colour).
    pub fn is_linked(&self) -> bool {
        self.component || self.instance_of.is_some()
    }

    /// Does the text differ from what `main` shows (so its measured size is its own)?
    fn own_text(&self, main: &Node) -> bool {
        (&self.text, &self.spans, &self.font, self.weight, self.italic, self.font_size, self.line_height, self.letter_spacing, self.list)
            != (&main.text, &main.spans, &main.font, main.weight, main.italic, main.font_size, main.line_height, main.letter_spacing, main.list)
    }
}

/// Turn a copy of a main component (its ids still those of the main) into an instance of it.
/// A main nested in it becomes an instance too; an instance nested in it stays one.
fn into_instance(n: &mut Node) {
    fn part(n: &mut Node) {
        n.source = Some(n.id);
        if std::mem::take(&mut n.component) {
            n.instance_of = Some(n.id);
        }
        n.children.iter_mut().for_each(part);
    }
    (n.instance_of, n.component, n.source) = (Some(n.id), false, None);
    n.children.iter_mut().for_each(part);
}

/// A plain frame again: nothing in it is linked to a component any more.
fn strip(n: &mut Node) {
    n.walk_mut(&mut |c| (c.component, c.instance_of, c.source) = (false, None, None));
}

fn index(n: &Node) -> HashMap<u64, &Node> {
    let mut out = HashMap::new();
    n.walk(&mut |c| {
        out.insert(c.id, c);
    });
    out
}

/// Lays the layers of the main out in the box of the instance.
struct Fit {
    /// The box of the instance.
    to: Rect,
    /// The instance is flipped against its main (left-right, top-bottom): the parts are laid
    /// out mirrored, and what they show is mirrored too.
    flip: (bool, bool),
}

impl Fit {
    /// Put `part` where `main` (the node it mirrors) lands in the instance: by the constraints
    /// of `main`, as the frame around it goes from the box `from` it has in the main to the box
    /// `to` it has here. Result: the box of the part (before mirroring), for what it holds.
    /// `item`: the frame lays its items out itself, so the part only comes along with the size
    /// it has in the main; its place follows when the instance is laid out.
    fn place(part: &mut Node, main: &Node, from: Rect, to: Rect, item: bool) -> Rect {
        let mut g = Node::new(0, main.kind, main.x, main.y);
        (g.w, g.h, g.rotation, g.wrap, g.align, g.fixed_h) = (main.w, main.h, main.rotation, main.wrap, main.align, main.fixed_h);
        if item {
            g.translate(to.0 - from.0, to.1 - from.1);
        } else {
            (g.constrain_h, g.constrain_v) = (main.constrain_h, main.constrain_v);
            layout::reflow(&mut g, from, to);
        }
        if main.kind == Kind::Text && part.own_text(main) {
            // Its size comes from its own content; keep that, anchored where the text aligns.
            if main.wrap {
                (part.x, part.w) = (g.x, g.w);
            } else {
                part.x = match main.align {
                    1 => g.x + (g.w - part.w) * 0.5,
                    2 => g.x + g.w - part.w,
                    _ => g.x,
                };
            }
            part.y = g.y;
            if main.fixed_h {
                part.h = g.h;
            }
        } else {
            (part.x, part.y, part.w, part.h) = (g.x, g.y, g.w, g.h);
        }
        part.rotation = main.rotation;
        (g.x, g.y, g.w, g.h)
    }

    /// Mirror a part (its children are not in it yet) the way the instance is flipped. Doing
    /// it twice undoes it, which is also how a part is read back in the terms of the main.
    fn mirror(&self, part: &mut Node) {
        let t = self.to;
        if self.flip.0 {
            part.mirror(t.0 + t.2 * 0.5, true);
        }
        if self.flip.1 {
            part.mirror(t.1 + t.3 * 0.5, false);
        }
    }
}

/// What rebuilding an instance needs besides the two states of its main.
struct Rebuild<'a> {
    old: &'a HashMap<u64, &'a Node>,
    /// The parts the instance had, by the node they mirror.
    pool: HashMap<u64, Node>,
    fit: Fit,
    /// Group id in the main → group id in this instance.
    groups: HashMap<u64, u64>,
    next_id: &'a mut u64,
    names: &'a mut BTreeMap<u64, String>,
}

impl Rebuild<'_> {
    /// The part for `main`, a layer of a frame that has the box `from` in the main and `to` here.
    /// `auto`: that frame has auto layout.
    fn part(&mut self, main: &Node, from: Rect, to: Rect, auto: bool) -> Node {
        let mut part = match self.pool.remove(&main.id) {
            Some(mut part) => {
                // Compare with the main as the main has it: unmirrored.
                self.fit.mirror(&mut part);
                if let Some(old) = self.old.get(&main.id) {
                    part.follow(old, main);
                }
                part
            }
            None => {
                *self.next_id += 1;
                let mut part = Node { id: *self.next_id, children: Vec::new(), ..main.clone() };
                part.source = Some(main.id);
                part
            }
        };
        part.kind = main.kind;
        part.name.clone_from(&main.name);
        part.clip = main.clip;
        (part.wrap, part.fixed_h) = (main.wrap, main.fixed_h);
        (part.constrain_h, part.constrain_v) = (main.constrain_h, main.constrain_v);
        part.layout.clone_from(&main.layout);
        (part.size_h, part.size_v, part.absolute) = (main.size_h, main.size_v, main.absolute);
        part.closed = main.closed;
        part.points.clone_from(&main.points);
        part.holes.clone_from(&main.holes);
        part.component = false;
        part.instance_of = if main.component { Some(main.id) } else { main.instance_of };
        part.group = match main.group {
            0 => 0,
            g => match self.groups.get(&g) {
                Some(mine) => *mine,
                None => {
                    *self.next_id += 1;
                    if let Some(name) = self.names.get(&g).cloned() {
                        self.names.insert(*self.next_id, name);
                    }
                    self.groups.insert(g, *self.next_id);
                    *self.next_id
                }
            },
        };
        let at = Fit::place(&mut part, main, from, to, auto && !main.absolute);
        self.fit.mirror(&mut part);
        part.children = main.children.iter().map(|c| self.part(c, (main.x, main.y, main.w, main.h), at, main.is_auto())).collect();
        part
    }
}

/// Bring one instance in line with its main, which went from `old` (indexed) to `new`. With
/// both the same state this only lays the parts out again, e.g. after the instance was resized.
fn sync_instance(inst: &mut Node, old: &HashMap<u64, &Node>, new: &Node, next_id: &mut u64, names: &mut BTreeMap<u64, String>) {
    fn drain(n: Node, pool: &mut HashMap<u64, Node>) {
        let mut n = n;
        for c in std::mem::take(&mut n.children) {
            drain(c, pool);
        }
        if let Some(s) = n.source {
            pool.insert(s, n);
        }
    }
    if let Some(old) = old.get(&new.id) {
        // An instance that was not resized keeps the size of its main.
        if (inst.w, inst.h) == (old.w, old.h) {
            (inst.w, inst.h) = (new.w, new.h);
        }
        inst.follow(old, new);
    }
    // The layout is the main's. Of the sizes only "fill" is the instance's own: that is about
    // the frame it sits in.
    inst.layout.clone_from(&new.layout);
    for horizontal in [true, false] {
        let own = match (new.sizing(horizontal), inst.sizing(horizontal)) {
            (_, Sizing::Fill) => Sizing::Fill,
            (Sizing::Hug, _) => Sizing::Hug,
            _ => Sizing::Fixed,
        };
        inst.set_sizing(horizontal, own);
    }
    let mut pool = HashMap::new();
    for c in std::mem::take(&mut inst.children) {
        drain(c, &mut pool);
    }
    let new_index = index(new);
    let mut groups = HashMap::new();
    for (source, part) in &pool {
        if let Some(m) = new_index.get(source).filter(|m| m.group != 0 && part.group != 0) {
            groups.entry(m.group).or_insert(part.group);
        }
    }
    let flip = (inst.flip_h != new.flip_h, inst.flip_v != new.flip_v);
    let (from, to) = ((new.x, new.y, new.w, new.h), (inst.x, inst.y, inst.w, inst.h));
    let mut rebuild = Rebuild { old, pool, fit: Fit { to, flip }, groups, next_id, names };
    inst.children = new.children.iter().map(|c| rebuild.part(c, from, to, new.is_auto())).collect();
    // A flipped instance of a main with auto layout runs the other way.
    for (flipped, horizontal) in [(flip.0, true), (flip.1, false)] {
        if flipped {
            layout::flip_layout(inst, horizontal);
        }
    }
    // A main with auto layout: the instance places its parts itself, around what they hold here
    // (a longer label makes a button that hugs it grow).
    layout::settle(inst);
}

impl Doc {
    /// A node on any page.
    pub fn find_all(&self, id: u64) -> Option<&Node> {
        self.all_tops().find_map(|n| n.find(id))
    }

    /// The main component with this id, on any page.
    pub fn main(&self, id: u64) -> Option<&Node> {
        self.find_all(id).filter(|n| n.component)
    }

    /// Index of the page that holds this node.
    pub fn page_of(&self, id: u64) -> Option<usize> {
        if self.find(id).is_some() {
            return Some(self.page);
        }
        self.pages.iter().position(|p| p.nodes.iter().any(|n| n.find(id).is_some()))
    }

    /// Every main component of the document: (page index, id, name), page by page.
    pub fn components(&self) -> Vec<(usize, u64, String)> {
        let mut out = Vec::new();
        for i in 0..self.page_count() {
            let nodes = if i == self.page { &self.nodes } else { &self.pages[i].nodes };
            for top in nodes {
                top.walk(&mut |n| {
                    if n.component {
                        out.push((i, n.id, n.name.clone()));
                    }
                });
            }
        }
        out
    }

    /// Is this node a part of an instance? Its place, size and existence are then decided by
    /// the main component: it can be restyled, but not moved, resized, deleted or re-parented.
    pub fn pinned(&self, id: u64) -> bool {
        self.find(id).is_some_and(|n| n.source.is_some())
    }

    /// Would a new layer in this frame end up in an instance (the frame is one, or a part of one)?
    pub fn in_instance(&self, id: u64) -> bool {
        self.find(id).is_some_and(|n| n.instance_of.is_some() || n.source.is_some())
    }

    /// The instance this node belongs to: the node itself, or the instance it is a part of.
    pub fn instance_root(&self, id: u64) -> Option<&Node> {
        self.lineage(id).into_iter().find(|n| n.is_instance())
    }

    /// Make a main component. A single frame becomes one as it is; anything else is wrapped in
    /// a new frame around it, in the same parent and at the same place in the stack.
    pub fn make_component(&mut self, ids: &[u64]) -> Result<u64, &'static str> {
        let mut roots = self.roots_of(ids);
        self.sort_by_stack(&mut roots);
        let Some(&top) = roots.last() else { return Err("Select something to make a component of.") };
        if roots.iter().any(|id| self.pinned(*id)) {
            return Err("Layers inside an instance can't become a component. Detach the instance first.");
        }
        if let [id] = roots[..] {
            match self.find(id) {
                Some(n) if n.component => return Err("This frame is already a main component."),
                Some(n) if n.kind == Kind::Frame && n.instance_of.is_none() => {
                    self.find_mut(id).unwrap().component = true;
                    return Ok(id);
                }
                _ => {}
            }
        }
        let parent = self.parent_of(top);
        if roots.iter().any(|id| self.parent_of(*id) != parent) {
            return Err("Select layers from the same frame to make a component of them.");
        }
        let b = roots
            .iter()
            .filter_map(|id| self.find(*id).map(|n| n.aabb()))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
            .ok_or("Select something to make a component of.")?;
        let id = self.alloc_id();
        let mut frame = Node::new(id, Kind::Frame, b.0, b.1);
        (frame.w, frame.h, frame.component) = ((b.2 - b.0).max(1.0), (b.3 - b.1).max(1.0), true);
        frame.name = match roots[..] {
            [one] => self.find(one).map_or_else(String::new, |n| n.name.clone()),
            _ => format!("Component {}", self.components().len() + 1),
        };
        // Right above the front-most layer; the layers then move in from below it.
        let at = self.child_ids(parent).iter().position(|c| *c == top).map_or(0, |i| i + 1);
        match parent.and_then(|p| self.find_mut(p)) {
            Some(p) => p.children.insert(at, frame),
            None => self.nodes.insert(at, Arc::new(frame)),
        }
        self.move_nodes(&roots, Some(id), None, 0);
        Ok(id)
    }

    /// A new instance of a main component, where the main is, with ids of its own. Not in the
    /// document yet.
    pub fn instantiate(&mut self, main: u64) -> Option<Node> {
        let mut n = self.main(main)?.clone();
        into_instance(&mut n);
        (n.locked, n.clip, n.group) = (false, false, 0);
        self.regroup(std::slice::from_mut(&mut n));
        self.reassign_ids(&mut n);
        Some(n)
    }

    /// Does `n` look like an instance of main `main`: every part mirrors a node of it?
    fn mirrors(&self, n: &Node, main: u64) -> bool {
        let Some(main) = self.main(main).map(index) else { return false };
        let stray = |d: &Node| !d.source.and_then(|s| main.get(&s)).is_some_and(|m| m.kind == d.kind);
        !n.children.iter().any(|c| c.any(&stray))
    }

    /// A node leaves the instance it was a part of (copied out of it, or the instance is
    /// detached) and stops mirroring the main. An instance nested in there stays an instance,
    /// now one of its own: its parts are re-pointed at the nodes of its own main.
    fn release(&self, n: &mut Node) {
        let mirrored = n.source.take().and_then(|s| self.find_all(s));
        let Some(inner) = n.instance_of else {
            for c in &mut n.children {
                self.release(c);
            }
            return;
        };
        match mirrored {
            // A main nested in the outer main: the parts already name its nodes.
            Some(m) if m.component && m.id == inner => {}
            // An instance in the outer main: its parts say which node of the inner main each one is.
            Some(m) if m.instance_of == Some(inner) => {
                let mut map = HashMap::new();
                m.walk(&mut |c| {
                    map.insert(c.id, c.source);
                });
                let mut ok = true;
                for c in &mut n.children {
                    c.walk_mut(&mut |d| {
                        d.source = d.source.and_then(|s| map.get(&s).copied().flatten());
                        ok &= d.source.is_some();
                    });
                }
                if !ok {
                    strip(n);
                }
            }
            _ => strip(n),
        }
    }

    /// Prepare a pasted or duplicated node, while it still has the ids it was copied with: a
    /// copy of a main component becomes an instance of it, a copy of an instance stays one, and
    /// what was copied out of an instance (or whose main is not in this document) becomes plain.
    /// Result `true`: keep its ids. That is a main that is no longer in the document (cut and
    /// pasted): it comes back as the same main, so its instances stay linked.
    pub fn settle_copy(&mut self, n: &mut Node) -> bool {
        fn settle(doc: &Doc, n: &mut Node) {
            if n.component && doc.main(n.id).is_some() {
                return into_instance(n);
            }
            if let Some(main) = n.instance_of {
                if !doc.mirrors(n, main) {
                    strip(n);
                }
                return;
            }
            for c in &mut n.children {
                settle(doc, c);
            }
        }
        if n.source.is_some() {
            self.release(n);
        }
        settle(self, n);
        let (mut main, mut free, mut max) = (false, true, 0);
        n.walk(&mut |c| {
            main |= c.component;
            free &= self.find_all(c.id).is_none();
            max = max.max(c.id);
        });
        if main && free {
            self.next_id = self.next_id.max(max);
        }
        main && free
    }

    /// Detach an instance: it becomes a plain frame that no longer follows the main.
    pub fn detach(&mut self, id: u64) -> bool {
        let Some(mut n) = self.find(id).filter(|n| n.is_instance()).cloned() else { return false };
        n.instance_of = None;
        for c in &mut n.children {
            self.release(c);
        }
        *self.find_mut(id).unwrap() = n;
        true
    }

    /// Drop the overrides of an instance, or of one part of it (with everything in that part).
    pub fn reset_overrides(&mut self, id: u64) -> bool {
        fn reset(n: &mut Node, main: &HashMap<u64, &Node>, root: Option<u64>) {
            if let Some(m) = root.or(n.source).and_then(|s| main.get(&s)) {
                n.adopt(m);
            }
            for c in &mut n.children {
                reset(c, main, None);
            }
        }
        let Some(mut inst) = self.instance_root(id).cloned() else { return false };
        let Some(main) = inst.instance_of.and_then(|m| self.main(m)).cloned() else { return false };
        let ix = index(&main);
        if id == inst.id {
            reset(&mut inst, &ix, Some(main.id));
        } else {
            let flip = (inst.flip_h != main.flip_h, inst.flip_v != main.flip_v);
            if let Some(part) = inst.find_mut(id) {
                reset(part, &ix, None);
                // The look of the main, as a flipped instance shows it.
                part.walk_mut(&mut |c| {
                    for horizontal in [true, false] {
                        if if horizontal { flip.0 } else { flip.1 } {
                            c.mirror_look(horizontal);
                        }
                    }
                });
            }
        }
        sync_instance(&mut inst, &ix, &main, &mut self.next_id, &mut self.group_names);
        let root = inst.id;
        let changed = self.find(root) != Some(&inst);
        if changed {
            *self.find_mut(root).unwrap() = inst;
        }
        changed
    }

    /// Lay the parts of an instance out again from its main, after its box changed. The
    /// instance need not be in the document yet. Result `false`: not an instance, or no main.
    pub fn fit_instance(&mut self, inst: &mut Node) -> bool {
        let Some(main) = inst.instance_of.filter(|_| inst.is_instance()).and_then(|m| self.main(m)).cloned() else { return false };
        sync_instance(inst, &index(&main), &main, &mut self.next_id, &mut self.group_names);
        true
    }

    /// `fit_instance` for an instance in the document. Result: whether anything changed.
    #[cfg(test)]
    pub fn relayout(&mut self, id: u64) -> bool {
        let Some(mut inst) = self.find(id).filter(|n| n.is_instance()).cloned() else { return false };
        if !self.fit_instance(&mut inst) {
            return false;
        }
        let changed = self.find(id) != Some(&inst);
        if changed {
            *self.find_mut(id).unwrap() = inst;
        }
        changed
    }

    /// Would putting `node` into frame `parent` make a component contain itself? That is the
    /// case when the frame is (in) a main that `node` holds an instance of, directly or through
    /// the mains it instantiates.
    pub fn would_cycle(&self, node: &Node, parent: Option<u64>) -> bool {
        let Some(parent) = parent else { return false };
        let mut used = Vec::new();
        node.walk(&mut |n| used.extend(n.instance_of.or(n.component.then_some(n.id))));
        if used.is_empty() {
            return false;
        }
        let around: Vec<u64> = self.lineage(parent).iter().filter(|n| n.component).map(|n| n.id).collect();
        around.iter().any(|m| used.iter().any(|u| u == m || self.depends_on(*u, *m, 0)))
    }

    /// Does main `main` hold `on` (an instance of it, or the main itself), at any remove?
    fn depends_on(&self, main: u64, on: u64, depth: usize) -> bool {
        let Some(n) = self.find_all(main) else { return false };
        let mut used = Vec::new();
        for c in &n.children {
            c.walk(&mut |d| used.extend(d.instance_of.or(d.component.then_some(d.id))));
        }
        used.sort_unstable();
        used.dedup();
        used.contains(&on) || (depth < SYNC_ROUNDS && used.iter().any(|u| self.depends_on(*u, on, depth + 1)))
    }

    /// Run `f` on the nodes that `pick` accepts, on every page. Nothing inside node `skip` is
    /// looked at. Top-level nodes without a match stay shared with the undo snapshots.
    fn each_picked(&mut self, pick: impl Fn(&Node) -> bool, skip: u64, mut f: impl FnMut(&mut Node, &mut u64, &mut BTreeMap<u64, String>)) {
        fn has(n: &Node, pick: &impl Fn(&Node) -> bool, skip: u64) -> bool {
            n.id != skip && (pick(n) || n.children.iter().any(|c| has(c, pick, skip)))
        }
        fn run(n: &mut Node, pick: &impl Fn(&Node) -> bool, skip: u64, f: &mut impl FnMut(&mut Node)) {
            if n.id == skip {
                return;
            }
            if pick(n) {
                return f(n);
            }
            for c in &mut n.children {
                run(c, pick, skip, f);
            }
        }
        let Doc { nodes, pages, next_id, group_names, .. } = self;
        for top in nodes.iter_mut().chain(pages.iter_mut().flat_map(|p| p.nodes.iter_mut())) {
            if has(top, &pick, skip) {
                run(Arc::make_mut(top), &pick, skip, &mut |n| f(n, next_id, group_names));
            }
        }
    }

    /// Bring instances in line after an edit; `pre` is the document before it. Every instance
    /// (on any page) of a main component that changed takes over the change, and an instance
    /// whose size changed is laid out again. Top-level nodes that the edit left alone are shared
    /// with `pre` and are not even looked at, so this costs nothing for unrelated edits.
    /// Result: whether anything was changed.
    pub fn sync_components(&mut self, pre: &Doc) -> bool {
        let mut before = pre.clone();
        let mut any = false;
        for round in 0..SYNC_ROUNDS {
            let (mains, resized) = self.sync_jobs(&before, round == 0);
            if mains.is_empty() && resized.is_empty() {
                break;
            }
            // A main that holds one of these instances changes in turn: the next round sees it.
            before = self.clone();
            for (old, new) in &mains {
                let (id, old) = (new.id, index(old));
                self.each_picked(|n| n.instance_of == Some(id) && n.source.is_none(), id, |n, next_id, names| sync_instance(n, &old, new, next_id, names));
            }
            for (id, main) in resized {
                if let Some(main) = self.main(main).cloned() {
                    let ix = index(&main);
                    self.each_picked(|n| n.id == id, main.id, |n, next_id, names| sync_instance(n, &ix, &main, next_id, names));
                }
            }
            any = true;
        }
        any
    }

    /// What changed since `before`: mains as (old, new), and (with `sizes`) instances whose size
    /// is no longer what it was, as (id, main id).
    fn sync_jobs(&self, before: &Doc, sizes: bool) -> (Vec<(Node, Node)>, Vec<(u64, u64)>) {
        let linked = |n: &Node| n.component || n.is_instance();
        let tops = |d: &Doc| d.all_tops().map(|n| n as *const Node).collect::<HashSet<_>>();
        let (was, is) = (tops(before), tops(self));
        let mut now: Vec<&Node> = Vec::new();
        for top in self.all_tops().filter(|t| !was.contains(&(*t as *const Node))) {
            top.walk(&mut |n| {
                if linked(n) {
                    now.push(n);
                }
            });
        }
        let (mut mains, mut resized) = (Vec::new(), Vec::new());
        if now.is_empty() {
            return (mains, resized);
        }
        let mut then: HashMap<u64, &Node> = HashMap::new();
        for top in before.all_tops().filter(|t| !is.contains(&(*t as *const Node))) {
            top.walk(&mut |n| {
                if linked(n) {
                    then.insert(n.id, n);
                }
            });
        }
        for n in now {
            let Some(old) = then.get(&n.id) else { continue };
            if n.component && old.component && *old != n {
                // Merely moved: the instances would come out the same.
                let mut moved = (*old).clone();
                moved.translate(n.x - old.x, n.y - old.y);
                if moved != *n {
                    mains.push(((*old).clone(), n.clone()));
                }
            } else if let (true, true, Some(main)) = (sizes, n.is_instance(), n.instance_of) {
                if (old.w, old.h) != (n.w, n.h) {
                    resized.push((n.id, main));
                }
            }
        }
        (mains, resized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Constraint;

    const RED: Option<[u8; 4]> = Some([255, 0, 0, 255]);
    const BLUE: Option<[u8; 4]> = Some([0, 0, 255, 255]);
    const GREEN: Option<[u8; 4]> = Some([0, 255, 0, 255]);

    fn add(doc: &mut Doc, kind: Kind, parent: Option<u64>, (x, y, w, h): (f32, f32, f32, f32)) -> u64 {
        let id = doc.alloc_id();
        let mut n = Node::new(id, kind, x, y);
        (n.w, n.h, n.fill) = (w, h, RED);
        if kind == Kind::Text {
            n.text = "Label".into();
        }
        doc.insert(n, parent);
        id
    }

    /// A button: main frame 1 (at 100,100, 200×80) with rect 2 (the whole frame) and text 3.
    fn button() -> Doc {
        let mut doc = Doc::default();
        add(&mut doc, Kind::Frame, None, (100.0, 100.0, 200.0, 80.0));
        add(&mut doc, Kind::Rect, Some(1), (100.0, 100.0, 200.0, 80.0));
        add(&mut doc, Kind::Text, Some(1), (120.0, 130.0, 60.0, 20.0));
        assert_eq!(doc.make_component(&[1]), Ok(1));
        doc
    }

    /// Place an instance of `main` with its corner at (x, y). Result: its id.
    fn place(doc: &mut Doc, main: u64, x: f32, y: f32, parent: Option<u64>) -> u64 {
        let mut n = doc.instantiate(main).unwrap();
        n.translate(x - n.x, y - n.y);
        let id = n.id;
        assert!(!doc.would_cycle(&n, parent));
        doc.insert(n, parent);
        id
    }

    /// The part of instance `inst` that mirrors node `source` of the main.
    fn part(doc: &Doc, inst: u64, source: u64) -> &Node {
        let mut found = None;
        doc.find_all(inst).unwrap().walk(&mut |n| {
            if n.source == Some(source) {
                found = Some(n);
            }
        });
        found.unwrap()
    }

    fn edit(doc: &mut Doc, f: impl FnOnce(&mut Doc)) -> bool {
        let pre = doc.clone();
        f(doc);
        doc.sync_components(&pre)
    }

    #[test]
    fn a_frame_or_a_selection_becomes_a_main_component() {
        let mut doc = button();
        assert!(doc.find(1).unwrap().component);
        assert_eq!(doc.make_component(&[1]), Err("This frame is already a main component."));
        assert_eq!(doc.components(), vec![(0, 1, "Frame 1".to_owned())]);
        // Loose layers are wrapped in a frame fitted around them, where they were in the stack.
        let a = add(&mut doc, Kind::Rect, None, (500.0, 0.0, 40.0, 40.0));
        let b = add(&mut doc, Kind::Rect, None, (520.0, 30.0, 60.0, 30.0));
        let c = add(&mut doc, Kind::Rect, None, (900.0, 0.0, 10.0, 10.0));
        let main = doc.make_component(&[b, a]).unwrap();
        let f = doc.find(main).unwrap();
        assert!(f.component && f.fill.is_none() && f.kind == Kind::Frame);
        assert_eq!((f.x, f.y, f.w, f.h), (500.0, 0.0, 80.0, 60.0));
        assert_eq!(doc.child_ids(Some(main)), vec![a, b]);
        assert_eq!(doc.child_ids(None), vec![1, main, c]);
        // Inside a frame the wrapper stays in that frame; one layer gives the wrapper its name.
        let one = doc.make_component(&[3]).unwrap();
        assert_eq!((doc.parent_of(one), doc.child_ids(Some(1)), doc.child_ids(Some(one))), (Some(1), vec![2, one], vec![3]));
        assert_eq!(doc.find(one).unwrap().name, doc.find(3).unwrap().name);
        // Layers from different frames don't make one component.
        assert!(doc.make_component(&[2, c]).is_err());
        // An instance is wrapped, not turned into a main itself.
        let inst = place(&mut doc, main, 0.0, 500.0, None);
        let wrapped = doc.make_component(&[inst]).unwrap();
        assert!(wrapped != inst && doc.parent_of(inst) == Some(wrapped) && doc.find(inst).unwrap().is_instance());
        assert!(doc.make_component(&[doc.find(inst).unwrap().children[0].id]).is_err());
    }

    #[test]
    fn an_instance_is_a_copy_that_remembers_what_it_mirrors() {
        let mut doc = button();
        let i = place(&mut doc, 1, 400.0, 300.0, None);
        let inst = doc.find(i).unwrap();
        assert!(inst.is_instance() && !inst.component && inst.instance_of == Some(1));
        assert_eq!((inst.x, inst.y, inst.w, inst.h), (400.0, 300.0, 200.0, 80.0));
        assert_eq!(inst.children.iter().map(|c| c.source).collect::<Vec<_>>(), vec![Some(2), Some(3)]);
        assert!(inst.children.iter().all(|c| c.id > 3 && !c.is_instance()));
        assert_eq!((part(&doc, i, 3).x, part(&doc, i, 3).y), (420.0, 330.0));
        assert!(doc.pinned(inst.children[0].id) && !doc.pinned(i) && !doc.pinned(2));
        assert_eq!(doc.instance_root(inst.children[1].id).map(|n| n.id), Some(i));
        assert_eq!(doc.instance_root(2).map(|n| n.id), None);
        assert!(doc.instantiate(2).is_none());
        // Nothing is dropped into an instance, and a plain click picks it as a whole.
        assert_eq!(doc.frame_at(450.0, 320.0, &[]), None);
        assert_eq!(doc.frame_at(150.0, 120.0, &[]), Some(1));
        assert_eq!(doc.hit(450.0, 320.0, 1.0), Some(i));
        assert_eq!(doc.hit_within(i, 430.0, 335.0, 1.0), Some(part(&doc, i, 3).id));
        assert_eq!(doc.hit_deep(430.0, 335.0, 1.0), Some(part(&doc, i, 3).id));
        assert_eq!(doc.hit(150.0, 120.0, 1.0), Some(2));
    }

    #[test]
    fn instances_follow_the_main_and_keep_what_was_overridden() {
        let mut doc = button();
        let a = place(&mut doc, 1, 400.0, 0.0, None);
        let b = place(&mut doc, 1, 400.0, 200.0, None);
        let (b_rect, b_text) = (part(&doc, b, 2).id, part(&doc, b, 3).id);
        // Overrides on b: the fill of the rect, the text. No main changed, nothing to do.
        assert!(!edit(&mut doc, |d| {
            d.find_mut(b_rect).unwrap().fill = GREEN;
            d.find_mut(b_text).unwrap().text = "Buy".into();
        }));
        let shared = doc.clone();
        assert!(edit(&mut doc, |d| {
            let rect = d.find_mut(2).unwrap();
            (rect.fill, rect.radius, rect.opacity) = (BLUE, 12.0, 0.5);
            let text = d.find_mut(3).unwrap();
            (text.text, text.font_size) = ("Submit".into(), 24.0);
            d.find_mut(1).unwrap().stroke = BLUE;
        }));
        let at = |i: u64, s: u64| part(&doc, i, s).clone();
        assert_eq!((at(a, 2).fill, at(a, 2).radius, at(a, 2).opacity), (BLUE, 12.0, 0.5));
        assert_eq!((at(a, 3).text.as_str(), at(a, 3).font_size), ("Submit", 24.0));
        // Property by property: what b overrode stays, the rest of the same layer follows.
        assert_eq!((at(b, 2).fill, at(b, 2).radius, at(b, 2).opacity), (GREEN, 12.0, 0.5));
        assert_eq!((at(b, 3).text.as_str(), at(b, 3).font_size), ("Buy", 24.0));
        assert_eq!((doc.find(a).unwrap().stroke, doc.find(b).unwrap().stroke), (BLUE, BLUE));
        // The instances keep their ids and place; an untouched top-level node is not copied.
        assert_eq!((at(b, 2).id, at(b, 3).id), (b_rect, b_text));
        assert_eq!((doc.find(b).unwrap().x, doc.find(b).unwrap().y), (400.0, 200.0));
        let loose = add(&mut doc, Kind::Rect, None, (0.0, 900.0, 5.0, 5.0));
        let shared2 = doc.clone();
        assert!(edit(&mut doc, |d| d.find_mut(2).unwrap().blur = 3.0));
        let top = |d: &Doc, id: u64| d.nodes.iter().find(|n| n.id == id).cloned().unwrap();
        assert!(Arc::ptr_eq(&top(&doc, loose), &top(&shared2, loose)) && !Arc::ptr_eq(&top(&doc, a), &top(&shared2, a)));
        assert_eq!(part(&shared, a, 2).fill, RED);
        // Moving the main changes nothing in its instances.
        let before = doc.clone();
        assert!(!edit(&mut doc, |d| d.find_mut(1).unwrap().translate(-50.0, 30.0)));
        assert!(Arc::ptr_eq(&top(&doc, a), &top(&before, a)));
        // Dropping an override: the value of the main again, and it follows from then on.
        assert!(doc.reset_overrides(b_rect));
        assert_eq!((at2(&doc, b, 2).fill, at2(&doc, b, 3).text.as_str()), (BLUE, "Buy"));
        assert!(doc.reset_overrides(b));
        assert_eq!(at2(&doc, b, 3).text, "Submit");
        assert!(!doc.reset_overrides(b) && !doc.reset_overrides(1));
        // An instance on another page follows too.
        doc.add_page();
        let far = place(&mut doc, 1, 0.0, 0.0, None);
        doc.switch_page(0);
        assert!(doc.find(far).is_none());
        assert!(edit(&mut doc, |d| d.find_mut(2).unwrap().fill = GREEN));
        assert_eq!(part(&doc, far, 2).fill, GREEN);
    }

    fn at2(doc: &Doc, inst: u64, source: u64) -> Node {
        part(doc, inst, source).clone()
    }

    #[test]
    fn structure_and_geometry_always_come_from_the_main() {
        let mut doc = button();
        let i = place(&mut doc, 1, 400.0, 300.0, None);
        let text = part(&doc, i, 3).id;
        // A layer added to the main, one removed, one moved and renamed, the order changed.
        let mut dot = 0;
        assert!(edit(&mut doc, |d| {
            dot = add(d, Kind::Ellipse, Some(1), (110.0, 110.0, 10.0, 10.0));
            let t = d.find_mut(3).unwrap();
            t.translate(30.0, -10.0);
            t.name = "Caption".into();
            d.reorder(&[3], false);
            d.remove(2);
        }));
        let inst = doc.find(i).unwrap();
        assert_eq!(inst.children.iter().map(|c| c.source).collect::<Vec<_>>(), vec![Some(3), Some(dot)]);
        assert_eq!((inst.children[0].id, inst.children[0].name.as_str()), (text, "Caption"));
        assert_eq!((inst.children[0].x, inst.children[0].y), (450.0, 320.0));
        let new = &inst.children[1];
        assert_eq!((new.kind, new.x, new.y, new.w), (Kind::Ellipse, 410.0, 310.0, 10.0));
        assert!(doc.find(new.id).is_some() && new.id != dot);
        // A part moved by hand goes back where the main has it; a group is mirrored as a group.
        assert!(edit(&mut doc, |d| {
            d.find_mut(text).unwrap().translate(500.0, 500.0);
            d.group(&[3, dot]);
        }));
        let inst = doc.find(i).unwrap();
        assert_eq!((inst.children[0].x, inst.children[0].y), (450.0, 320.0));
        let (g, main_g) = (inst.children[0].group, doc.find(3).unwrap().group);
        assert!(g != 0 && g != main_g && inst.children[1].group == g);
        assert_eq!(doc.group_name(g), doc.group_name(main_g));
        // A frame nested in the main arrives with what it holds.
        let mut inner = 0;
        assert!(edit(&mut doc, |d| {
            inner = add(d, Kind::Frame, Some(1), (200.0, 110.0, 50.0, 50.0));
            add(d, Kind::Rect, Some(inner), (210.0, 120.0, 10.0, 10.0));
        }));
        let copy = part(&doc, i, inner);
        assert_eq!((copy.children.len(), copy.children[0].x, copy.children[0].y), (1, 510.0, 320.0));
        assert!(!copy.is_linked());
    }

    #[test]
    fn a_resized_instance_lays_its_parts_out_again() {
        let mut doc = button();
        // Both layers scale with the frame.
        for id in [2, 3] {
            let n = doc.find_mut(id).unwrap();
            (n.constrain_h, n.constrain_v) = (Constraint::Scale, Constraint::Scale);
        }
        let i = place(&mut doc, 1, 400.0, 300.0, None);
        // Twice as wide, half as high; the size change alone lays it out again.
        assert!(edit(&mut doc, |d| {
            let n = d.find_mut(i).unwrap();
            (n.w, n.h) = (400.0, 40.0);
        }));
        let (rect, text) = (at2(&doc, i, 2), at2(&doc, i, 3));
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (400.0, 300.0, 400.0, 40.0));
        // Auto-width text keeps its size; its corner moves along. In the main: (20, 30) of 200×80.
        assert_eq!((text.x, text.y, text.w, text.h), (440.0, 315.0, 60.0, 20.0));
        assert!(!doc.relayout(i));
        // A change in the main lands in the stretched box; the instance keeps its size.
        assert!(edit(&mut doc, |d| {
            let r = d.find_mut(2).unwrap();
            (r.x, r.w, r.fill) = (150.0, 100.0, BLUE);
        }));
        let rect = at2(&doc, i, 2);
        assert_eq!((rect.x, rect.w, rect.h, rect.fill), (500.0, 200.0, 40.0, BLUE));
        assert_eq!((doc.find(i).unwrap().w, doc.find(i).unwrap().h), (400.0, 40.0));
        // An instance that was not resized takes the new size of its main.
        let plain = place(&mut doc, 1, 0.0, 600.0, None);
        assert!(edit(&mut doc, |d| d.find_mut(1).unwrap().w = 260.0));
        assert_eq!((doc.find(plain).unwrap().w, doc.find(i).unwrap().w), (260.0, 400.0));
        // Text with content of its own keeps its measured size, anchored where the main has it.
        let label = part(&doc, plain, 3).id;
        edit(&mut doc, |d| {
            let t = d.find_mut(label).unwrap();
            (t.text, t.w) = ("A much longer label".into(), 180.0);
        });
        assert!(edit(&mut doc, |d| d.find_mut(3).unwrap().translate(5.0, 5.0)));
        let t = doc.find(label).unwrap();
        assert_eq!((t.x, t.y, t.w, t.text.as_str()), (25.0, 635.0, 180.0, "A much longer label"));
    }

    #[test]
    fn parts_follow_the_constraints_of_the_main() {
        use Constraint::*;
        let mut doc = button();
        let rect_of = |d: &Doc, i: u64, s: u64| {
            let n = part(d, i, s);
            (n.x, n.y, n.w, n.h)
        };
        let pin = |d: &mut Doc, id: u64, c: (Constraint, Constraint)| {
            let n = d.find_mut(id).unwrap();
            (n.constrain_h, n.constrain_v) = c;
        };
        // The background fills the button, the label stays at the top left, a badge in the
        // bottom right corner, and a bar across the middle with a knob at its right end.
        pin(&mut doc, 2, (Stretch, Stretch));
        let badge = add(&mut doc, Kind::Ellipse, Some(1), (270.0, 150.0, 20.0, 20.0));
        pin(&mut doc, badge, (Max, Max));
        let bar = add(&mut doc, Kind::Frame, Some(1), (150.0, 110.0, 100.0, 20.0));
        pin(&mut doc, bar, (Stretch, Center));
        let knob = add(&mut doc, Kind::Rect, Some(bar), (240.0, 110.0, 10.0, 20.0));
        pin(&mut doc, knob, (Max, Stretch));
        let i = place(&mut doc, 1, 400.0, 300.0, None);
        // The size of the main: everything where the main has it.
        assert_eq!((rect_of(&doc, i, badge), rect_of(&doc, i, knob)), ((570.0, 350.0, 20.0, 20.0), (540.0, 310.0, 10.0, 20.0)));
        assert!(edit(&mut doc, |d| {
            let n = d.find_mut(i).unwrap();
            (n.w, n.h) = (300.0, 100.0);
        }));
        assert_eq!(rect_of(&doc, i, 2), (400.0, 300.0, 300.0, 100.0));
        assert_eq!(rect_of(&doc, i, 3), (420.0, 330.0, 60.0, 20.0));
        assert_eq!(rect_of(&doc, i, badge), (670.0, 370.0, 20.0, 20.0));
        // The bar is wider and stays 20 above the middle; the knob follows the end of the bar.
        assert_eq!((rect_of(&doc, i, bar), rect_of(&doc, i, knob)), ((450.0, 320.0, 200.0, 20.0), (640.0, 320.0, 10.0, 20.0)));
        // The parts carry the constraints of the main, and a change of them arrives like any other.
        assert_eq!((part(&doc, i, badge).constrain_h, part(&doc, i, knob).constrain_v), (Max, Stretch));
        assert!(edit(&mut doc, |d| pin(d, badge, (Min, Max))));
        assert_eq!((rect_of(&doc, i, badge), part(&doc, i, badge).constrain_h), ((570.0, 370.0, 20.0, 20.0), Min));
        // The constraint of the instance itself, in the frame it sits in, is its own.
        let own = doc.find_mut(i).unwrap();
        own.constrain_h = Center;
        assert!(edit(&mut doc, |d| d.find_mut(2).unwrap().fill = BLUE));
        assert_eq!(doc.find(i).unwrap().constrain_h, Center);
    }

    #[test]
    fn a_flipped_instance_stays_the_mirror_image_of_its_main() {
        let mut doc = button();
        // An arrow in the main, pointing right, near its right edge.
        let arrow = add(&mut doc, Kind::Path, Some(1), (260.0, 130.0, 20.0, 20.0));
        let p = |x: f32, y: f32| crate::doc::PathPoint { x, y, hin: [0.0; 2], hout: [0.0; 2], r: None };
        doc.find_mut(arrow).unwrap().points = vec![p(0.0, 0.0), p(1.0, 0.5), p(0.0, 1.0)];
        let i = place(&mut doc, 1, 400.0, 300.0, None);
        assert!(!edit(&mut doc, |d| assert!(d.flip(&[i], true))), "flipping an instance asks nothing of the others");
        let at = |d: &Doc, s: u64| part(d, i, s).clone();
        let tips = |n: &Node| n.points.iter().map(|q| q.x).collect::<Vec<_>>();
        // Text at 20 from the left in the main: 20 from the right here, and mirrored.
        assert_eq!((at(&doc, 3).x, at(&doc, 3).flip_h, doc.find(i).unwrap().flip_h), (520.0, true, true));
        assert_eq!((at(&doc, arrow).x, tips(&at(&doc, arrow))), (420.0, vec![1.0, 0.0, 1.0]));
        // An override on the mirrored text, then changes in the main: the layout stays mirrored,
        // the override stays, and everything else follows.
        let text = at(&doc, 3).id;
        assert!(edit(&mut doc, |d| {
            d.find_mut(text).unwrap().fill = GREEN;
            let m = d.find_mut(3).unwrap();
            (m.x, m.font_size, m.fill) = (110.0, 24.0, BLUE);
            d.find_mut(arrow).unwrap().points[1].y = 0.25;
            d.find_mut(2).unwrap().flip_v = true;
        }));
        let t = at(&doc, 3);
        assert_eq!((t.x, t.font_size, t.fill, t.flip_h, t.flip_v), (530.0, 24.0, GREEN, true, false));
        let a = at(&doc, arrow);
        assert_eq!((a.x, tips(&a), a.points[1].y, a.flip_h), (420.0, vec![1.0, 0.0, 1.0], 0.25, false));
        assert_eq!((at(&doc, 2).flip_h, at(&doc, 2).flip_v), (true, true));
        // Resizing lays it out again, still mirrored.
        assert!(edit(&mut doc, |d| d.find_mut(i).unwrap().w = 400.0));
        // (Both keep their distance to the left edge of the main, which is the right edge here.)
        assert_eq!((at(&doc, 3).x, at(&doc, 3).w, at(&doc, arrow).x), (730.0, 60.0, 620.0));
        // Flipped back, it is laid out like the main again; the override is still there.
        doc.find_mut(i).unwrap().w = 200.0;
        assert!(doc.flip(&[i], true) && doc.relayout(i));
        let t = at(&doc, 3);
        assert_eq!((t.x, t.flip_h, t.fill, tips(&at(&doc, arrow))), (410.0, false, GREEN, vec![0.0, 1.0, 0.0]));
        // Dropping the overrides of a flipped instance makes it plain again.
        assert!(doc.flip(&[i], false));
        assert!(doc.reset_overrides(i));
        assert_eq!((doc.find(i).unwrap().flip_v, at(&doc, 3).flip_v, at(&doc, 3).y, at(&doc, 3).fill), (false, false, 330.0, BLUE));
    }

    #[test]
    fn a_change_passes_through_nested_components() {
        let mut doc = button();
        // Main `card` holds an instance of the button; `page` holds an instance of the card.
        let card = add(&mut doc, Kind::Frame, None, (0.0, 1000.0, 400.0, 300.0));
        doc.make_component(&[card]).unwrap();
        let inner = place(&mut doc, 1, 20.0, 1020.0, Some(card));
        assert_eq!(doc.parent_of(inner), Some(card));
        let page = add(&mut doc, Kind::Frame, None, (1000.0, 0.0, 800.0, 600.0));
        let outer = place(&mut doc, card, 1100.0, 100.0, Some(page));
        // In the instance of the card the button is still an instance, managed by the card.
        let nested = part(&doc, outer, inner).clone();
        assert!(nested.instance_of == Some(1) && !nested.is_instance() && nested.children.len() == 2);
        assert_eq!((nested.x, nested.y), (1120.0, 120.0));
        let deep_rect = part(&doc, outer, part(&doc, inner, 2).id).id;
        let deep_text = part(&doc, outer, part(&doc, inner, 3).id).id;
        edit(&mut doc, |d| d.find_mut(deep_text).unwrap().text = "Mine".into());
        // One edit of the button reaches the card, and through it the instance of the card.
        assert!(edit(&mut doc, |d| {
            d.find_mut(2).unwrap().fill = BLUE;
            d.find_mut(3).unwrap().text = "Go".into();
        }));
        assert_eq!((part(&doc, inner, 2).fill, part(&doc, inner, 3).text.as_str()), (BLUE, "Go"));
        assert_eq!((doc.find(deep_rect).unwrap().fill, doc.find(deep_text).unwrap().text.as_str()), (BLUE, "Mine"));
        // Detaching the outer instance leaves the button in it an instance of its own.
        assert!(doc.detach(outer));
        let freed = doc.find(nested.id).unwrap();
        assert!(freed.is_instance() && freed.children.iter().map(|c| c.source).eq([Some(2), Some(3)]));
        assert!(!doc.find(outer).unwrap().is_linked());
        assert!(edit(&mut doc, |d| d.find_mut(2).unwrap().fill = GREEN));
        assert_eq!((doc.find(deep_rect).unwrap().fill, doc.find(deep_text).unwrap().text.as_str()), (GREEN, "Mine"));
        // A main nested in a main comes along as an instance of it.
        let badge = add(&mut doc, Kind::Frame, Some(card), (300.0, 1010.0, 40.0, 40.0));
        let dot = add(&mut doc, Kind::Ellipse, Some(badge), (310.0, 1020.0, 20.0, 20.0));
        doc.make_component(&[badge]).unwrap();
        let again = place(&mut doc, card, 0.0, 2000.0, None);
        let copy = part(&doc, again, badge).clone();
        assert!(copy.instance_of == Some(badge) && !copy.component && copy.children[0].source == Some(dot));
        assert!(edit(&mut doc, |d| d.find_mut(dot).unwrap().fill = BLUE));
        assert_eq!(part(&doc, again, dot).fill, BLUE);
    }

    #[test]
    fn a_component_cannot_end_up_inside_itself() {
        let mut doc = button();
        let card = add(&mut doc, Kind::Frame, None, (0.0, 1000.0, 400.0, 300.0));
        doc.make_component(&[card]).unwrap();
        place(&mut doc, 1, 20.0, 1020.0, Some(card));
        let plain = add(&mut doc, Kind::Frame, Some(1), (110.0, 110.0, 50.0, 50.0));
        let of_button = doc.instantiate(1).unwrap();
        let of_card = doc.instantiate(card).unwrap();
        // Into itself, also through a plain frame in it; into a main that it is instantiated in.
        assert!(doc.would_cycle(&of_button, Some(1)));
        assert!(doc.would_cycle(&of_button, Some(plain)));
        assert!(doc.would_cycle(&of_card, Some(card)));
        // The card holds a button: an instance of the card in the button would hold itself.
        assert!(doc.would_cycle(&of_card, Some(plain)));
        // The other way round is how it already is; the top level and plain frames are free.
        assert!(!doc.would_cycle(&of_button, Some(card)));
        assert!(!doc.would_cycle(&of_card, None));
        let free = add(&mut doc, Kind::Frame, None, (5000.0, 0.0, 900.0, 900.0));
        assert!(!doc.would_cycle(&of_card, Some(free)));
        // The mains themselves, and frames holding instances, count the same way.
        assert!(doc.would_cycle(&doc.find(card).unwrap().clone(), Some(plain)));
        let mut holder = Node::new(0, Kind::Frame, 0.0, 0.0);
        holder.children.push(of_card);
        assert!(doc.would_cycle(&holder, Some(1)));
        assert!(!doc.would_cycle(&Node::new(0, Kind::Rect, 0.0, 0.0), Some(1)));
        // Should a cycle exist anyway (an old or hand-made file), syncing still ends.
        let mut bad = doc.instantiate(1).unwrap();
        bad.translate(10.0, 10.0);
        doc.insert(bad, Some(1));
        edit(&mut doc, |d| d.find_mut(2).unwrap().fill = BLUE);
        assert!(doc.count() < 200);
    }

    #[test]
    fn detached_and_orphaned_instances_stop_following() {
        let mut doc = button();
        let a = place(&mut doc, 1, 400.0, 0.0, None);
        let b = place(&mut doc, 1, 400.0, 200.0, None);
        let c = place(&mut doc, 1, 400.0, 400.0, None);
        assert!(doc.detach(a) && !doc.detach(a) && !doc.detach(1));
        let plain = doc.find(a).unwrap();
        assert!(!plain.any(&|n| n.is_linked() || n.source.is_some()) && plain.children.len() == 2);
        assert!(!doc.detach(part(&doc, b, 2).id));
        assert!(edit(&mut doc, |d| d.find_mut(2).unwrap().fill = BLUE));
        assert_eq!((doc.find(a).unwrap().children[0].fill, part(&doc, b, 2).fill), (RED, BLUE));
        // The main is no longer a component: b keeps what it has and stops following.
        edit(&mut doc, |d| d.find_mut(1).unwrap().component = false);
        assert!(doc.main(1).is_none() && doc.find(b).unwrap().is_instance());
        assert!(!edit(&mut doc, |d| d.find_mut(2).unwrap().fill = GREEN));
        assert_eq!(part(&doc, b, 2).fill, BLUE);
        assert!(!doc.reset_overrides(b) && !doc.relayout(b));
        // The same when the main is deleted; detaching still works.
        edit(&mut doc, |d| {
            d.remove(1);
        });
        assert!(doc.find(c).unwrap().instance_of == Some(1) && doc.instantiate(1).is_none());
        assert!(doc.detach(c) && !doc.find(c).unwrap().any(&|n| n.is_linked() || n.source.is_some()));
    }

    #[test]
    fn copies_of_components_become_instances() {
        let mut doc = button();
        let copy = |doc: &mut Doc, id: u64| -> (Node, bool) {
            let mut n = doc.find_all(id).unwrap().clone();
            let keep = doc.settle_copy(&mut n);
            if !keep {
                doc.reassign_ids(&mut n);
            }
            (n, keep)
        };
        // A copy of the main is an instance of it; a copy of that is another instance.
        let (first, keep) = copy(&mut doc, 1);
        assert!(!keep && first.is_instance() && first.instance_of == Some(1) && first.children[1].source == Some(3));
        let i = first.id;
        doc.insert(first, None);
        let (second, _) = copy(&mut doc, i);
        assert!(second.is_instance() && second.id != i && second.children[0].source == Some(2));
        // A part copied out of an instance is a plain layer.
        let label = part(&doc, i, 3).id;
        let (loose, _) = copy(&mut doc, label);
        assert!(loose.source.is_none() && !loose.is_linked());
        // A frame holding a main: the copy holds an instance.
        let holder = add(&mut doc, Kind::Frame, None, (0.0, 0.0, 900.0, 900.0));
        doc.move_nodes(&[1], Some(holder), None, 0);
        let (held, _) = copy(&mut doc, holder);
        assert!(!held.is_linked() && held.children[0].is_instance() && held.children[0].instance_of == Some(1));
        // Cut and pasted, the main comes back as itself, so its instances stay linked.
        let cut = doc.remove(1).unwrap();
        let mut back = cut.clone();
        assert!(doc.settle_copy(&mut back));
        assert!(back.component && back.id == 1 && back.children[0].id == 2);
        doc.insert(back, None);
        assert!(edit(&mut doc, |d| d.find_mut(2).unwrap().fill = BLUE));
        assert_eq!(part(&doc, i, 2).fill, BLUE);
        assert!(doc.alloc_id() > doc.max_node_id() - 1);
        // In a document without the main, a pasted instance is a plain frame.
        let mut other = Doc::default();
        add(&mut other, Kind::Frame, None, (0.0, 0.0, 10.0, 10.0));
        let mut pasted = doc.find(i).unwrap().clone();
        assert!(!other.settle_copy(&mut pasted));
        assert!(!pasted.any(&|n| n.is_linked() || n.source.is_some()));
        // And one with an unrelated main of the same id is not mistaken for its instance.
        other.find_mut(1).unwrap().component = true;
        let mut pasted = doc.find(i).unwrap().clone();
        other.settle_copy(&mut pasted);
        assert!(!pasted.is_linked());
    }

    #[test]
    fn a_duplicated_page_gets_mains_of_its_own() {
        let mut doc = button();
        let i = place(&mut doc, 1, 400.0, 0.0, None);
        assert_eq!(doc.duplicate_page(0), Some(1));
        // The copied main is a new, independent main; the copied instance still follows the original.
        let mains = doc.components();
        assert_eq!(mains.iter().map(|m| m.0).collect::<Vec<_>>(), vec![0, 1]);
        let copy = doc.nodes.iter().find(|n| n.is_instance()).map(|n| n.id).unwrap();
        assert!(copy != i && doc.find(copy).unwrap().instance_of == Some(1) && mains[1].1 != 1);
        assert_eq!(doc.page_of(1), Some(0));
        assert_eq!(doc.page_of(copy), Some(1));
        let original_rect = 2;
        assert!(edit(&mut doc, |d| {
            d.switch_page(0);
            d.find_mut(original_rect).unwrap().fill = BLUE;
        }));
        assert_eq!((part(&doc, i, 2).fill, part(&doc, copy, 2).fill), (BLUE, BLUE));
        assert_eq!(doc.find_all(mains[1].1).unwrap().children[0].fill, RED);
    }

    #[test]
    fn components_survive_saving_and_old_files_still_open() {
        let mut doc = button();
        let i = place(&mut doc, 1, 400.0, 0.0, None);
        let json = serde_json::to_string(&doc).unwrap();
        let mut back: Doc = serde_json::from_str(&json).unwrap();
        assert_eq!(back.nodes, doc.nodes);
        assert!(back.find(1).unwrap().component && back.find(i).unwrap().is_instance());
        assert!(edit(&mut back, |d| d.find_mut(2).unwrap().fill = BLUE));
        assert_eq!(part(&back, i, 2).fill, BLUE);
        // The new fields are only written where they say something.
        let plain = serde_json::to_string(&Node::new(1, Kind::Rect, 0.0, 0.0)).unwrap();
        assert!(!plain.contains("component") && !plain.contains("instance_of") && !plain.contains("source"));
        // A file from before components.
        let old: Doc = serde_json::from_str(r#"{"nodes":[{"id":1,"name":"Frame 1","kind":"Frame","x":0,"y":0,"w":10,"h":10,"children":[{"id":2,"name":"R","kind":"Rect","x":0,"y":0,"w":5,"h":5}]}],"next_id":2}"#).unwrap();
        let n = old.find(2).unwrap();
        assert!(!old.nodes[0].is_linked() && n.source.is_none() && old.components().is_empty());
    }
}
