//! Find and replace in the text layers of a document: where a query occurs, and replacing
//! occurrences while the styled character ranges keep their place. Everything here works on
//! the document alone; the window and the highlights on the canvas are in `app/find.rs`.
//!
//! All positions are character indices (as in `Span`), never bytes.

use crate::doc::{Doc, Kind, Node};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Options {
    /// "a" does not find "A".
    pub match_case: bool,
    /// The match may not touch a letter, digit or underscore on either side.
    pub whole_word: bool,
}

/// Where to look.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Scope {
    /// The current page.
    Page,
    /// Every page, in page order.
    All,
    /// These layers of the current page, and everything inside those that are frames.
    Within(Vec<u64>),
}

/// One occurrence: characters [start, end) of the text of node `id` on page `page`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Match {
    pub page: usize,
    pub id: u64,
    pub start: usize,
    pub end: usize,
}

/// One character with its case folded away, for comparing without regard to case. Lowercasing
/// a string can change its number of characters ("İ" becomes two), which would shift every
/// index behind it, so this folds character by character and leaves the rare character whose
/// other case is not a single character as it is. Going through the upper case first makes
/// "ς" and "σ" (both "Σ") the same.
fn fold(c: char) -> char {
    fn single(mut it: impl Iterator<Item = char>, or: char) -> char {
        match (it.next(), it.next()) {
            (Some(one), None) => one,
            _ => or,
        }
    }
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let upper = single(c.to_uppercase(), c);
    single(upper.to_lowercase(), upper)
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A query ready to be looked for in many texts.
pub struct Needle {
    chars: Vec<char>,
    opt: Options,
}

impl Needle {
    pub fn new(query: &str, opt: Options) -> Self {
        let chars = if opt.match_case { query.chars().collect() } else { query.chars().map(fold).collect() };
        Needle { chars, opt }
    }

    /// The occurrences in `text` as character ranges, in order and without overlap ("aa" is
    /// found twice in "aaaa"). An empty query is found nowhere.
    /// The characters of `text` as they are compared with the query.
    fn hay(&self, text: &str) -> Vec<char> {
        if self.opt.match_case {
            text.chars().collect()
        } else {
            text.chars().map(fold).collect()
        }
    }

    /// Does the query occur in `hay` (see `hay`) at character `i`?
    fn at(&self, hay: &[char], i: usize) -> bool {
        let m = self.chars.len();
        if m == 0 || i + m > hay.len() || hay[i..i + m] != self.chars[..] {
            return false;
        }
        // A side of the query that is not itself part of a word (a query like "(a)") needs
        // no boundary there.
        !self.opt.whole_word || ((i == 0 || !word_char(hay[i - 1]) || !word_char(self.chars[0])) && (i + m == hay.len() || !word_char(hay[i + m]) || !word_char(self.chars[m - 1])))
    }

    pub fn find_in(&self, text: &str) -> Vec<(usize, usize)> {
        let m = self.chars.len();
        // (Bytes are at least as many as characters: a text shorter than the query in bytes
        // can't hold it. Most layers are ruled out here without being decoded.)
        if m == 0 || text.len() < m {
            return Vec::new();
        }
        let hay = self.hay(text);
        let mut out = Vec::new();
        let mut i = 0;
        while i + m <= hay.len() {
            if self.at(&hay, i) {
                out.push((i, i + m));
                i += m;
            } else {
                i += 1;
            }
        }
        out
    }
}

/// Replace these character ranges of the text of `n` (in order, not overlapping, as `Needle::find_in`
/// gives them) with `with`. The styled ranges follow (see `Node::replace_range`), and so does a
/// layer name that was taken from the text. Ranges that do not fit the text are left out, and
/// with `check` (the query the ranges were found with) so are those where the text no longer
/// says what was searched for: the text changed since the search, and replacing by position
/// would cut into whatever stands there now. Result: how many were replaced.
pub fn replace_in(n: &mut Node, ranges: &[(usize, usize)], with: &str, check: Option<&Needle>) -> usize {
    let auto_name = |text: &str| -> String { text.lines().next().unwrap_or("Text").chars().take(40).collect() };
    let named_by_text = n.name == auto_name(&n.text);
    let len = n.text.chars().count();
    // (Compared with the text as it is now: replacing from the end does not shift what is before.)
    let hay = check.map(|c| (c, c.hay(&n.text)));
    let found = |a: usize, b: usize| hay.as_ref().is_none_or(|(c, hay)| b - a == c.chars.len() && c.at(hay, a));
    let mut done = 0;
    let mut limit = len;
    // From the last to the first: the indices of those still to come are not shifted.
    for &(a, b) in ranges.iter().rev() {
        if a < b && b <= limit && found(a, b) {
            n.replace_range(a, b, with);
            limit = a;
            done += 1;
        }
    }
    if done > 0 && named_by_text && !n.text.trim().is_empty() {
        n.name = auto_name(&n.text);
    }
    done
}

/// Every occurrence of `query` in the text layers of `scope`, page by page and within a page
/// in stacking order (as the Layers panel lists them, read from the bottom).
///
/// Hidden and locked layers, and whatever is inside a hidden or locked frame, are left out
/// altogether: what is found can always be shown on the canvas and replaced.
pub fn find_all(doc: &Doc, query: &str, opt: Options, scope: &Scope) -> Vec<Match> {
    let needle = Needle::new(query, opt);
    let mut out = Vec::new();
    if needle.chars.is_empty() {
        return out;
    }
    fn visit(n: &Node, page: usize, needle: &Needle, within: Option<&[u64]>, inside: bool, out: &mut Vec<Match>) {
        if !n.visible || n.locked {
            return;
        }
        let inside = inside || within.is_none_or(|ids| ids.contains(&n.id));
        if inside && n.kind == Kind::Text {
            out.extend(needle.find_in(&n.text).into_iter().map(|(start, end)| Match { page, id: n.id, start, end }));
        }
        for c in &n.children {
            visit(c, page, needle, within, inside, out);
        }
    }
    let within = match scope {
        Scope::Within(ids) => Some(ids.as_slice()),
        _ => None,
    };
    for page in 0..doc.page_count() {
        let current = page == doc.page || doc.pages.is_empty();
        if !current && *scope != Scope::All {
            continue;
        }
        let nodes = if current { &doc.nodes } else { &doc.pages[page].nodes };
        for top in nodes {
            visit(top, page, &needle, within, false, &mut out);
        }
    }
    out
}

/// Replace these matches (from `find_all` on this document with `check`, the query) with `with`.
/// Matches that the text no longer holds are skipped (see `replace_in`). A text that would be
/// left with nothing to show is removed, as the text editor removes one that is left empty; a
/// part of an instance can't be removed, so there such a replacement is not made at all.
/// Top-level layers without a match stay shared with the undo snapshots. Result: how many
/// matches were replaced, the ids of the layers whose text changed, and the ids of those removed.
pub fn replace_matches(doc: &mut Doc, matches: &[Match], with: &str, check: Option<&Needle>) -> (usize, Vec<u64>, Vec<u64>) {
    let mut by_node: HashMap<u64, Vec<(usize, usize)>> = HashMap::new();
    for m in matches {
        by_node.entry(m.id).or_default().push((m.start, m.end));
    }
    for ranges in by_node.values_mut() {
        ranges.sort_unstable();
    }
    let (mut count, mut changed, mut removed) = (0, Vec::new(), Vec::new());
    doc.update_nodes(
        |n| n.kind == Kind::Text && by_node.contains_key(&n.id),
        |n| {
            let mut new = n.clone();
            let done = replace_in(&mut new, &by_node[&n.id], with, check);
            let emptied = new.text.trim().is_empty();
            if done == 0 || (emptied && n.source.is_some()) {
                return;
            }
            count += done;
            if emptied { &mut removed } else { &mut changed }.push(n.id);
            *n = new;
        },
    );
    if !removed.is_empty() {
        let gone: HashSet<u64> = removed.iter().copied().collect();
        fn prune(n: &mut Node, gone: &HashSet<u64>) {
            n.children.retain(|c| !gone.contains(&c.id));
            n.children.iter_mut().for_each(|c| prune(c, gone));
        }
        let lists = std::iter::once(&mut doc.nodes).chain(doc.pages.iter_mut().map(|p| &mut p.nodes));
        for list in lists {
            list.retain(|n| !gone.contains(&n.id));
            for top in list.iter_mut().filter(|top| top.any(&|n| gone.contains(&n.id))) {
                prune(Arc::make_mut(top), &gone);
            }
        }
    }
    (count, changed, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Page, Span, Style};

    const PLAIN: Options = Options { match_case: false, whole_word: false };
    const CASE: Options = Options { match_case: true, whole_word: false };
    const WORD: Options = Options { match_case: false, whole_word: true };

    fn find_in(text: &str, query: &str, opt: Options) -> Vec<(usize, usize)> {
        Needle::new(query, opt).find_in(text)
    }

    fn text(id: u64, s: &str) -> Node {
        let mut n = Node::new(id, Kind::Text, 0.0, 0.0);
        n.text = s.into();
        n.name = s.lines().next().unwrap_or_default().into();
        n
    }

    fn bold(n: &Node) -> Style {
        Style { weight: 700, ..n.base_style() }
    }

    /// The styled ranges as (start, end, the characters they cover).
    fn spans(n: &Node) -> Vec<(usize, usize, String)> {
        n.spans.iter().map(|s| (s.start, s.end, n.text.chars().skip(s.start).take(s.end - s.start).collect())).collect()
    }

    #[test]
    fn finds_every_occurrence_without_overlap() {
        assert_eq!(find_in("one two one", "one", PLAIN), [(0, 3), (8, 11)]);
        assert_eq!(find_in("aaaa", "aa", PLAIN), [(0, 2), (2, 4)]);
        assert_eq!(find_in("aaa", "aa", PLAIN), [(0, 2)]);
        assert_eq!(find_in("line\nbreak", "e\nb", PLAIN), [(3, 6)]);
        assert!(find_in("short", "much longer", PLAIN).is_empty());
    }

    #[test]
    fn an_empty_query_finds_nothing() {
        assert!(find_in("anything", "", PLAIN).is_empty());
        assert!(find_in("", "", PLAIN).is_empty());
        let mut doc = Doc::default();
        doc.insert(text(1, "anything"), None);
        assert!(find_all(&doc, "", PLAIN, &Scope::All).is_empty());
    }

    #[test]
    fn case_is_ignored_unless_asked_for() {
        assert_eq!(find_in("Kanvas kanvas KANVAS", "kanvas", PLAIN), [(0, 6), (7, 13), (14, 20)]);
        assert_eq!(find_in("Kanvas kanvas KANVAS", "kanvas", CASE), [(7, 13)]);
        assert_eq!(find_in("Kanvas kanvas KANVAS", "KANVAS", PLAIN).len(), 3);
    }

    #[test]
    fn positions_are_characters_also_where_case_changes_length() {
        // "é" is two bytes; the indices count it once.
        assert_eq!(find_in("Café CAFÉ café", "café", PLAIN), [(0, 4), (5, 9), (10, 14)]);
        assert_eq!(find_in("ÜBER über", "Über", PLAIN), [(0, 4), (5, 9)]);
        // "İ" lowercases to two characters; what follows it must not shift.
        assert_eq!(find_in("İİ end END", "end", PLAIN), [(3, 6), (7, 10)]);
        // "ß" uppercases to two characters: it stays itself, and is found as itself.
        assert_eq!(find_in("Straße STRASSE straße", "straße", PLAIN), [(0, 6), (15, 21)]);
        // Both lowercase sigmas are the same letter.
        assert_eq!(find_in("ΟΔΟΣ οδος οδοσ", "οδος", PLAIN), [(0, 4), (5, 9), (10, 14)]);
        // Wide characters and emoji are one character each.
        assert_eq!(find_in("日本語 🙂 text", "text", PLAIN), [(6, 10)]);
    }

    #[test]
    fn whole_word_needs_a_boundary_on_both_sides() {
        assert_eq!(find_in("cat concat cats cat_1 Cat.", "cat", WORD), [(0, 3), (22, 25)]);
        assert_eq!(find_in("über überall", "über", WORD), [(0, 4)]);
        assert_eq!(find_in("a1 a 1a", "a", WORD), [(3, 4)]);
        // A query that ends in punctuation needs no boundary behind it.
        assert_eq!(find_in("Hi! Hi!there hi", "hi!", WORD), [(0, 3), (4, 7)]);
        assert_eq!(find_in("ok\nok", "ok", WORD), [(0, 2), (3, 5)]);
    }

    #[test]
    fn replacing_several_matches_in_one_text() {
        let mut n = text(1, "red, red and red");
        let found = find_in(&n.text, "red", PLAIN);
        assert_eq!(replace_in(&mut n, &found, "green", None), 3);
        assert_eq!(n.text, "green, green and green");
        // The name was the text, so it follows.
        assert_eq!(n.name, "green, green and green");
        let mut named = text(2, "red");
        named.name = "Title".into();
        replace_in(&mut named, &[(0, 3)], "blue", None);
        assert_eq!((named.text.as_str(), named.name.as_str()), ("blue", "Title"));
        // Ranges that do not fit the text (a stale search) change nothing.
        let mut short = text(3, "red");
        assert_eq!(replace_in(&mut short, &[(2, 9)], "x", None), 0);
        assert_eq!(short.text, "red");
    }

    #[test]
    fn styled_ranges_keep_their_characters() {
        // "Buy two, get two free": "two, get" bold (4..12), "free" bold (17..21).
        let mut n = text(1, "Buy two, get two free");
        let style = bold(&n);
        n.spans = vec![Span { start: 4, end: 12, style: style.clone() }, Span { start: 17, end: 21, style: style.clone() }];
        let found = find_in(&n.text, "two", PLAIN);
        assert_eq!(found, [(4, 7), (13, 16)]);
        replace_in(&mut n, &found, "three", None);
        assert_eq!(n.text, "Buy three, get three free");
        // The first match was bold and stays bold, the second was not; "free" moved by four.
        assert_eq!(spans(&n), [(4, 14, "three, get".into()), (21, 25, "free".into())]);

        // A match that starts before a range and ends inside it: the new text has the style of
        // the first character replaced (plain), the rest of the range keeps its own.
        let mut n = text(2, "abcdef");
        n.spans = vec![Span { start: 2, end: 5, style: style.clone() }];
        n.replace_range(1, 3, "XYZ");
        assert_eq!(n.text, "aXYZdef");
        assert_eq!(spans(&n), [(4, 6, "de".into())]);
        // A match that starts inside a range and runs past its end: the new text is bold.
        let mut n = text(3, "abcdef");
        n.spans = vec![Span { start: 1, end: 3, style: style.clone() }];
        n.replace_range(2, 5, "X");
        assert_eq!(n.text, "abXf");
        assert_eq!(spans(&n), [(1, 3, "bX".into())]);
        // A range wholly inside the match is gone; one that is exactly the match stays.
        let mut n = text(4, "abcdef");
        n.spans = vec![Span { start: 2, end: 3, style: style.clone() }, Span { start: 4, end: 6, style: style.clone() }];
        n.replace_range(1, 4, "-");
        n.replace_range(2, 4, "long");
        assert_eq!(n.text, "a-long");
        assert_eq!(spans(&n), [(2, 6, "long".into())]);
        // Non-ASCII text: the indices are characters.
        let mut n = text(5, "Señor café olé");
        n.spans = vec![Span { start: 11, end: 14, style }];
        let found = find_in(&n.text, "CAFÉ", PLAIN);
        replace_in(&mut n, &found, "té", None);
        assert_eq!(n.text, "Señor té olé");
        assert_eq!(spans(&n), [(9, 12, "olé".into())]);
    }

    #[test]
    fn replacing_with_nothing_removes_the_matches() {
        let mut n = text(1, "a--b--c");
        n.spans = vec![Span { start: 1, end: 3, style: bold(&n) }, Span { start: 3, end: 4, style: Style { italic: true, ..n.base_style() } }];
        let found = find_in(&n.text, "--", PLAIN);
        assert_eq!(replace_in(&mut n, &found, "", None), 2);
        assert_eq!(n.text, "abc");
        assert_eq!(spans(&n), [(1, 2, "b".into())]);
        // A text emptied altogether keeps its name (`replace_matches` removes such a layer).
        let mut gone = text(2, "word");
        replace_in(&mut gone, &[(0, 4)], "", None);
        assert_eq!((gone.text.as_str(), gone.name.as_str()), ("", "word"));
    }

    #[test]
    fn stale_matches_are_skipped_instead_of_cutting_into_the_text() {
        let needle = Needle::new("now", PLAIN);
        let mut n = text(1, "now and now");
        let found = needle.find_in(&n.text);
        // The text changed under the matches (an instance followed its main): the first is
        // still there, where the second was the text says something else.
        n.text = "now or tomorrow".into();
        assert_eq!(replace_in(&mut n.clone(), &found, "then", None), 2);
        assert_eq!(replace_in(&mut n, &found, "then", Some(&needle)), 1);
        assert_eq!(n.text, "then or tomorrow");
        // The right place, but not as a whole word any more; and a range of another length.
        let word = Needle::new("cat", WORD);
        let mut n = text(2, "cats");
        assert_eq!((replace_in(&mut n, &[(0, 3)], "dog", Some(&word)), n.text.as_str()), (0, "cats"));
        assert_eq!(replace_in(&mut n, &[(0, 4)], "dog", Some(&Needle::new("cat", PLAIN))), 0);
        assert_eq!((replace_in(&mut n, &[(0, 3)], "dog", Some(&Needle::new("CAT", PLAIN))), n.text.as_str()), (1, "dogs"));
    }

    #[test]
    fn replacing_one_by_one_in_a_main_and_its_instance() {
        // The window replaces the current match, searches again and goes on, while the instance
        // only follows its main at the end of the frame (`Doc::settle`): the matches found in
        // the instance in between are those of its old text.
        let mut doc = Doc::default();
        doc.insert(Node::new(1, Kind::Frame, 0.0, 0.0), None);
        doc.insert(text(2, "now and now"), Some(1));
        doc.next_id = 10;
        assert_eq!(doc.make_component(&[1]), Ok(1));
        let inst = doc.instantiate(1).unwrap();
        let label = inst.children[0].id;
        doc.insert(inst, None);
        let needle = Needle::new("now", PLAIN);
        let stale = find_all(&doc, "now", PLAIN, &Scope::Page);
        assert_eq!(stale.iter().map(|m| (m.id, m.start)).collect::<Vec<_>>(), [(2, 0), (2, 8), (label, 0), (label, 8)]);
        let pre = doc.clone();
        assert_eq!(replace_matches(&mut doc, &stale[..1], "tomorrow", Some(&needle)), (1, vec![2], vec![]));
        doc.settle(Some(&pre), &[]);
        assert_eq!(doc.find(label).unwrap().text, "tomorrow and now");
        // The old second match of the instance, (8, 11), is "d n" now: not replaced.
        assert_eq!(replace_matches(&mut doc, &stale[3..], "tomorrow", Some(&needle)), (0, vec![], vec![]));
        assert_eq!(doc.find(label).unwrap().text, "tomorrow and now");
        // Searched again, the rest goes one by one.
        for _ in 0..4 {
            let pre = doc.clone();
            let found = find_all(&doc, "now", PLAIN, &Scope::Page);
            replace_matches(&mut doc, &found[..found.len().min(1)], "tomorrow", Some(&needle));
            doc.settle(Some(&pre), &[]);
        }
        assert_eq!((doc.find(2).unwrap().text.as_str(), doc.find(label).unwrap().text.as_str()), ("tomorrow and tomorrow", "tomorrow and tomorrow"));
    }

    #[test]
    fn a_text_left_empty_is_removed_unless_it_is_part_of_an_instance() {
        let mut doc = sample();
        doc.find_mut(5).unwrap().text = " hello ".into();
        doc.pages[1].nodes = vec![Arc::new(text(9, "hello"))];
        let found = find_all(&doc, "hello", PLAIN, &Scope::All);
        let (count, changed, mut removed) = replace_matches(&mut doc, &found, "", None);
        removed.sort_unstable();
        // "Hello world" keeps a word; "hello hello" keeps a space only, " hello " two.
        assert_eq!((count, changed, removed), (5, vec![1], vec![3, 5, 9]));
        assert_eq!(doc.find(1).unwrap().text, " world");
        assert!(doc.find(3).is_none() && doc.find(5).is_none() && doc.find(4).is_some() && doc.pages[1].nodes.is_empty());
        // The label of an instance stays as it is; the one of the main goes, and with it
        // (once the document is settled) the part of the instance.
        let mut doc = Doc::default();
        doc.insert(Node::new(1, Kind::Frame, 0.0, 0.0), None);
        doc.insert(text(2, "Buy"), Some(1));
        doc.next_id = 10;
        assert_eq!(doc.make_component(&[1]), Ok(1));
        let inst = doc.instantiate(1).unwrap();
        let (i, label) = (inst.id, inst.children[0].id);
        doc.insert(inst, None);
        let found = find_all(&doc, "buy", PLAIN, &Scope::Within(vec![i]));
        assert_eq!(replace_matches(&mut doc, &found, "", None), (0, vec![], vec![]));
        assert_eq!(doc.find(label).unwrap().text, "Buy");
        let pre = doc.clone();
        let found = find_all(&doc, "buy", PLAIN, &Scope::Within(vec![1]));
        assert_eq!(replace_matches(&mut doc, &found, " ", None), (1, vec![], vec![2]));
        doc.settle(Some(&pre), &[]);
        assert!(doc.find(2).is_none() && doc.find(label).is_none() && doc.find(i).is_some());
    }

    /// Page 1 (current): text 1, frame 2 { text 3, frame 4 { text 5 } }, hidden text 6, locked
    /// frame 7 { text 8 }. Page 2: text 9.
    fn sample() -> Doc {
        let mut doc = Doc::default();
        doc.insert(text(1, "Hello world"), None);
        doc.insert(Node::new(2, Kind::Frame, 0.0, 0.0), None);
        doc.insert(text(3, "hello hello"), Some(2));
        doc.insert(Node::new(4, Kind::Frame, 0.0, 0.0), Some(2));
        doc.insert(text(5, "Deep hello"), Some(4));
        let mut hidden = text(6, "hidden hello");
        hidden.visible = false;
        doc.insert(hidden, None);
        let mut locked = Node::new(7, Kind::Frame, 0.0, 0.0);
        locked.locked = true;
        doc.insert(locked, None);
        doc.insert(text(8, "locked hello"), Some(7));
        // A rectangle named like the query is not a text.
        let mut rect = Node::new(10, Kind::Rect, 0.0, 0.0);
        (rect.name, rect.text) = ("hello".into(), "hello".into());
        doc.insert(rect, None);
        doc.next_id = 20;
        doc.pages = vec![
            Page { id: 11, name: "Page 1".into(), ..Default::default() },
            Page { id: 12, name: "Page 2".into(), nodes: vec![Arc::new(text(9, "Other page: hello"))], ..Default::default() },
        ];
        doc
    }

    #[test]
    fn scope_decides_where_to_look() {
        let doc = sample();
        let ids = |scope: Scope| -> Vec<(usize, u64, usize)> { find_all(&doc, "hello", PLAIN, &scope).iter().map(|m| (m.page, m.id, m.start)).collect() };
        // Hidden and locked layers are left out, and so is what a locked frame holds.
        assert_eq!(ids(Scope::Page), [(0, 1, 0), (0, 3, 0), (0, 3, 6), (0, 5, 5)]);
        assert_eq!(ids(Scope::All), [(0, 1, 0), (0, 3, 0), (0, 3, 6), (0, 5, 5), (1, 9, 12)]);
        // A selected frame brings everything inside it, at any depth.
        assert_eq!(ids(Scope::Within(vec![2])), [(0, 3, 0), (0, 3, 6), (0, 5, 5)]);
        assert_eq!(ids(Scope::Within(vec![4, 1])), [(0, 1, 0), (0, 5, 5)]);
        assert!(ids(Scope::Within(vec![])).is_empty());
        assert!(ids(Scope::Within(vec![7, 6])).is_empty());
        // A document that has no page list yet is one page.
        let mut single = Doc::default();
        single.insert(text(1, "hello"), None);
        assert_eq!(find_all(&single, "HELLO", PLAIN, &Scope::All).len(), 1);
        assert_eq!(find_all(&single, "HELLO", CASE, &Scope::Page).len(), 0);
    }

    #[test]
    fn instances_take_a_replacement_as_an_override_and_follow_their_main() {
        // Main component 1 with label 2, and two instances of it; the second has its own text.
        let mut doc = Doc::default();
        let mut frame = Node::new(1, Kind::Frame, 0.0, 0.0);
        (frame.w, frame.h) = (200.0, 80.0);
        doc.insert(frame, None);
        doc.insert(text(2, "Buy now"), Some(1));
        doc.next_id = 10;
        assert_eq!(doc.make_component(&[1]), Ok(1));
        let place = |doc: &mut Doc, y: f32| {
            let mut n = doc.instantiate(1).unwrap();
            n.translate(0.0, y);
            let id = n.id;
            doc.insert(n, None);
            id
        };
        let (a, b) = (place(&mut doc, 100.0), place(&mut doc, 200.0));
        let label = |doc: &Doc, frame: u64| doc.find(frame).unwrap().children[0].clone();
        let own = label(&doc, b).id;
        doc.find_mut(own).unwrap().text = "Buy later".into();
        let labels = |doc: &Doc| [label(doc, 1).text, label(doc, a).text, label(doc, b).text];
        let replace = |doc: &mut Doc, query: &str, with: &str, scope: Scope| {
            let pre = doc.clone();
            let found = find_all(doc, query, PLAIN, &scope);
            let (count, ..) = replace_matches(doc, &found, with, None);
            // What the app does at the end of the frame.
            doc.settle(Some(&pre), &[]);
            count
        };
        // In the main only: the instance that shows the main's text follows, the other keeps its own.
        assert_eq!(replace(&mut doc, "buy", "Get", Scope::Within(vec![1])), 1);
        assert_eq!(labels(&doc), ["Get now", "Get now", "Buy later"]);
        // On the whole page: the main and the instance are both replaced, to the same text.
        assert_eq!(replace(&mut doc, "now", "today", Scope::Page), 2);
        assert_eq!(labels(&doc), ["Get today", "Get today", "Buy later"]);
        // Which is no override: that instance still follows the main.
        assert_eq!(replace(&mut doc, "get", "Take", Scope::Within(vec![1])), 1);
        assert_eq!(labels(&doc), ["Take today", "Take today", "Buy later"]);
        // In a part of an instance: an override, kept when the main changes.
        assert_eq!(replace(&mut doc, "today", "it", Scope::Within(vec![a])), 1);
        assert_eq!(replace(&mut doc, "take", "Grab", Scope::Within(vec![1])), 1);
        assert_eq!(labels(&doc), ["Grab today", "Take it", "Buy later"]);
    }

    #[test]
    fn replace_all_reaches_every_page_and_copies_only_what_changes() {
        let mut doc = sample();
        let before = doc.clone();
        let found = find_all(&doc, "hello", PLAIN, &Scope::All);
        let (count, mut changed, _) = replace_matches(&mut doc, &found, "bye", None);
        changed.sort_unstable();
        assert_eq!((count, changed), (5, vec![1, 3, 5, 9]));
        assert_eq!(doc.find(1).unwrap().text, "bye world");
        assert_eq!(doc.find(3).unwrap().text, "bye bye");
        assert_eq!(doc.find(5).unwrap().text, "Deep bye");
        assert_eq!(doc.pages[1].nodes[0].text, "Other page: bye");
        // Left alone: hidden, locked, and the rectangle.
        assert_eq!(doc.find(6).unwrap().text, "hidden hello");
        assert_eq!(doc.find(8).unwrap().text, "locked hello");
        assert_eq!(doc.find(10).unwrap().text, "hello");
        // Top-level layers without a match are still the ones of the snapshot.
        let shared = |d: &Doc, id: u64| d.nodes.iter().find(|n| n.id == id).map(Arc::as_ptr);
        for id in [6, 7, 10] {
            assert_eq!(shared(&doc, id), shared(&before, id), "node {id} was copied");
        }
        assert_ne!(shared(&doc, 2), shared(&before, 2));
        // The snapshot itself is untouched (this is what undo restores).
        assert_eq!(before.find(3).unwrap().text, "hello hello");
        assert!(find_all(&doc, "hello", PLAIN, &Scope::All).is_empty());
        // One match only: the others in the same text stay.
        let mut doc = sample();
        let found = find_all(&doc, "hello", PLAIN, &Scope::Page);
        assert_eq!(replace_matches(&mut doc, &found[2..3], "you", None), (1, vec![3], vec![]));
        assert_eq!(doc.find(3).unwrap().text, "hello you");
    }
}
