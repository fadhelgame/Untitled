//! The `.kanvas` file.
//!
//! A file is a list of records behind an 8-byte magic. Each record is a 28-byte head (tag, id,
//! payload length, checksum; little endian) followed by its payload:
//!
//! - `IMG ` — the original bytes of one image; `id` is its key in `Doc::assets`.
//! - `DOC ` — the document as JSON, with the image bytes left out. The last one in the file is
//!   the document; earlier ones are old versions waiting to be cleaned up.
//!
//! Images never change once added, so saving again only appends what is new: unseen images and
//! one `DOC ` record. A large document is therefore not rewritten on every auto-save. Old
//! versions pile up at a bounded rate and are dropped by writing the whole file afresh (see
//! `Stored::append_ok`). An append that is cut short leaves the previous `DOC ` record intact:
//! the reader stops at the first incomplete record.
//!
//! Files written before this format are a single JSON object with the images embedded as
//! base64; they are still read, and are converted the first time they are saved.

use crate::doc::{Asset, Doc};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAGIC: &[u8; 8] = b"KANVAS2\n";
const IMAGE: [u8; 4] = *b"IMG ";
const DOCUMENT: [u8; 4] = *b"DOC ";
const HEAD: u64 = 28;
/// Below this size the whole file is written every time: it is quick, and keeps small files small.
const SMALL: u64 = 8 << 20;

/// What is known about a file on disk, so that the next save can append to it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stored {
    /// Where the last complete `DOC ` record ends; the next record goes here.
    end: u64,
    /// Images already in the file.
    images: BTreeSet<u64>,
    /// Size of the current `DOC ` record, head included, and its checksum.
    doc: u64,
    sum: u64,
    /// Bytes taken by old versions and by images the document no longer has.
    dead: u64,
}

impl Stored {
    /// The file holds old versions that a full write would drop.
    pub fn garbage(&self) -> bool {
        self.dead > 0
    }

    /// Appending is worth it for a large file, as long as old versions stay under a quarter of it.
    fn append_ok(&self) -> bool {
        let live = self.end.saturating_sub(self.dead);
        live >= SMALL && self.dead + self.doc <= live / 4
    }
}

fn head(tag: [u8; 4], id: u64, len: u64, sum: u64) -> [u8; HEAD as usize] {
    let mut h = [0; HEAD as usize];
    h[..4].copy_from_slice(&tag);
    h[4..12].copy_from_slice(&id.to_le_bytes());
    h[12..20].copy_from_slice(&len.to_le_bytes());
    h[20..28].copy_from_slice(&sum.to_le_bytes());
    h
}

fn word(h: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(h[at..at + 8].try_into().unwrap())
}

/// FNV-1a; tells a completely written `DOC ` record from one that was cut short.
fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// The document as JSON, its images reduced to their keys.
fn json(doc: &Doc) -> Result<Vec<u8>, String> {
    let empty = Arc::new(Asset { bytes: Vec::new() });
    let mut lean = doc.clone();
    lean.assets = doc.assets.keys().map(|&k| (k, empty.clone())).collect();
    serde_json::to_vec(&lean).map_err(|e| e.to_string())
}

fn temp(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

/// Write `doc` to `path`. With `known` (what the last read or save of this same path returned)
/// only the new parts are appended when that is possible; otherwise the file is written afresh
/// through a temporary file that then replaces the old one. Safe to call from any thread.
pub fn save(doc: &Doc, path: &Path, known: Option<&Stored>) -> Result<Stored, String> {
    let json = json(doc)?;
    if let Some(known) = known.filter(|k| k.append_ok()) {
        if let Some(stored) = append(doc, &json, path, known).map_err(|e| e.to_string())? {
            return Ok(stored);
        }
    }
    let tmp = temp(path);
    rewrite(doc, &json, path, &tmp).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

fn rewrite(doc: &Doc, json: &[u8], path: &Path, tmp: &Path) -> io::Result<Stored> {
    let mut out = BufWriter::with_capacity(1 << 20, File::create(tmp)?);
    out.write_all(MAGIC)?;
    let mut end = MAGIC.len() as u64;
    for (&id, asset) in &doc.assets {
        out.write_all(&head(IMAGE, id, asset.bytes.len() as u64, 0))?;
        out.write_all(&asset.bytes)?;
        end += HEAD + asset.bytes.len() as u64;
    }
    let sum = checksum(json);
    out.write_all(&head(DOCUMENT, 0, json.len() as u64, sum))?;
    out.write_all(json)?;
    let size = HEAD + json.len() as u64;
    let file = out.into_inner().map_err(|e| e.into_error())?;
    file.sync_data()?;
    drop(file);
    std::fs::rename(tmp, path)?;
    Ok(Stored { end: end + size, images: doc.assets.keys().copied().collect(), doc: size, sum, dead: 0 })
}

/// Add the new images and the document to the end of the file. `None` = the file is not the
/// one `known` describes (replaced or removed by something else), so it has to be rewritten.
fn append(doc: &Doc, json: &[u8], path: &Path, known: &Stored) -> io::Result<Option<Stored>> {
    let Ok(mut file) = std::fs::OpenOptions::new().read(true).write(true).open(path) else { return Ok(None) };
    let mut last = [0; HEAD as usize];
    let same = file.metadata()?.len() >= known.end
        && known.doc >= HEAD
        && file.seek(SeekFrom::Start(known.end - known.doc)).is_ok()
        && file.read_exact(&mut last).is_ok()
        && last == head(DOCUMENT, 0, known.doc - HEAD, known.sum);
    if !same {
        return Ok(None);
    }
    // Drop whatever an interrupted save may have left behind the last complete record.
    file.set_len(known.end)?;
    file.seek(SeekFrom::Start(known.end))?;
    let mut out = BufWriter::with_capacity(1 << 20, file);
    let mut stored = known.clone();
    for (&id, asset) in &doc.assets {
        if stored.images.insert(id) {
            out.write_all(&head(IMAGE, id, asset.bytes.len() as u64, 0))?;
            out.write_all(&asset.bytes)?;
            stored.end += HEAD + asset.bytes.len() as u64;
        }
    }
    stored.sum = checksum(json);
    out.write_all(&head(DOCUMENT, 0, json.len() as u64, stored.sum))?;
    out.write_all(json)?;
    out.into_inner().map_err(|e| e.into_error())?.sync_data()?;
    stored.dead += known.doc;
    stored.doc = HEAD + json.len() as u64;
    stored.end += stored.doc;
    Ok(Some(stored))
}

/// Read a document from `path`. The second value is `None` for a file in the old JSON format.
/// Safe to call from any thread.
pub fn read(path: &Path) -> Result<(Doc, Option<Stored>), String> {
    let (mut doc, stored) = read_file(path)?;
    // New ids must not collide with the ids in the file.
    doc.next_id = doc.next_id.max(doc.max_node_id());
    doc.fix_pages();
    Ok((doc, stored))
}

fn read_file(path: &Path) -> Result<(Doc, Option<Stored>), String> {
    let text = |e: io::Error| e.to_string();
    let mut file = File::open(path).map_err(text)?;
    let len = file.metadata().map_err(text)?.len();
    let mut magic = Vec::with_capacity(MAGIC.len());
    Read::by_ref(&mut file).take(MAGIC.len() as u64).read_to_end(&mut magic).map_err(text)?;
    if magic != MAGIC {
        // The old format: one JSON object.
        let mut bytes = magic;
        bytes.reserve(len as usize);
        file.read_to_end(&mut bytes).map_err(text)?;
        let doc = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        return Ok((doc, None));
    }

    let mut images: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    // Images read since the last complete `DOC ` record; they only count once one follows.
    let mut fresh: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut current: Option<Vec<u8>> = None;
    let mut stored = Stored { end: MAGIC.len() as u64, ..Stored::default() };
    let mut skipped = 0;
    let mut pos = MAGIC.len() as u64;
    let mut h = [0; HEAD as usize];
    while pos + HEAD <= len && file.read_exact(&mut h).is_ok() {
        let (id, size) = (word(&h, 4), word(&h, 12));
        if size > len - pos - HEAD {
            break;
        }
        let tag: [u8; 4] = h[..4].try_into().unwrap();
        if tag == IMAGE || tag == DOCUMENT {
            let mut payload = vec![0; size as usize];
            if file.read_exact(&mut payload).is_err() {
                break;
            }
            if tag == IMAGE {
                fresh.push((id, payload));
            } else {
                let sum = checksum(&payload);
                if sum != word(&h, 20) {
                    break;
                }
                images.extend(fresh.drain(..));
                stored.dead += stored.doc + std::mem::take(&mut skipped);
                stored.doc = HEAD + size;
                stored.sum = sum;
                stored.end = pos + HEAD + size;
                current = Some(payload);
            }
        } else {
            // A record from a newer version: step over it.
            if file.seek(SeekFrom::Current(size as i64)).is_err() {
                break;
            }
            skipped += HEAD + size;
        }
        pos += HEAD + size;
    }
    let json = current.ok_or("the file is incomplete: it holds no document")?;
    let mut doc: Doc = serde_json::from_slice(&json).map_err(|e| e.to_string())?;
    drop(json);
    stored.images = images.keys().copied().collect();
    let wanted: Vec<u64> = doc.assets.keys().copied().collect();
    for id in wanted {
        match images.remove(&id) {
            Some(bytes) => {
                doc.assets.insert(id, Arc::new(Asset { bytes }));
            }
            None => {
                doc.assets.remove(&id);
            }
        }
    }
    // Images of older versions that the document no longer uses.
    stored.dead += images.values().map(|b| HEAD + b.len() as u64).sum::<u64>();
    Ok((doc, Some(stored)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Kind, Node};

    fn path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kanvas_test_{name}_{}.kanvas", std::process::id()))
    }

    fn sample() -> (Doc, u64) {
        let mut doc = Doc::default();
        let image = doc.add_asset(vec![7; 3000]);
        let mut frame = Node::new(doc.alloc_id(), Kind::Frame, 0.0, 0.0);
        frame.children.push(Node::new(doc.alloc_id(), Kind::Rect, 10.0, 10.0));
        doc.insert(frame, None);
        (doc, image)
    }

    fn size(path: &Path) -> u64 {
        std::fs::metadata(path).unwrap().len()
    }

    #[test]
    fn save_and_read_round_trip() {
        let (doc, image) = sample();
        let path = path("round");
        save(&doc, &path, None).unwrap();
        // Saving again replaces the file and leaves no temporary file behind.
        let stored = save(&doc, &path, None).unwrap();
        assert!(!temp(&path).exists());
        assert_eq!(stored.end, size(&path));
        let (back, known) = read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(known, Some(stored));
        assert_eq!(back.count(), 2);
        assert_eq!(back.nodes[0].children, doc.nodes[0].children);
        assert_eq!(back.assets[&image].bytes, vec![7; 3000]);
        assert!(read(&path).is_err());
    }

    #[test]
    fn old_json_files_are_read_and_converted() {
        let (doc, image) = sample();
        let path = path("old");
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let (back, known) = read(&path).unwrap();
        assert!(known.is_none());
        assert_eq!(back.assets[&image].bytes, vec![7; 3000]);
        save(&back, &path, known.as_ref()).unwrap();
        let (again, known) = read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(known.is_some());
        assert_eq!(again.count(), 2);
        assert_eq!(again.assets[&image].bytes, vec![7; 3000]);
    }

    #[test]
    fn a_small_file_is_rewritten_each_time() {
        let (mut doc, _) = sample();
        let path = path("small");
        let first = save(&doc, &path, None).unwrap();
        let id = doc.alloc_id();
        doc.insert(Node::new(id, Kind::Rect, 5.0, 5.0), None);
        let second = save(&doc, &path, Some(&first)).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(!second.garbage());
    }

    #[test]
    fn a_large_file_grows_by_what_changed() {
        let (mut doc, image) = sample();
        let big = doc.add_asset((0..SMALL as usize + 1000).map(|i| (i % 251) as u8).collect());
        let path = path("large");
        let first = save(&doc, &path, None).unwrap();
        let before = size(&path);

        // A changed layer appends one document record; the images stay where they are.
        let id = doc.alloc_id();
        doc.insert(Node::new(id, Kind::Rect, 5.0, 5.0), None);
        let second = save(&doc, &path, Some(&first)).unwrap();
        assert_eq!(size(&path), before + second.doc);
        assert!(second.garbage());
        let (back, known) = read(&path).unwrap();
        assert_eq!(known.as_ref(), Some(&second));
        assert_eq!(back.count(), 3);
        assert_eq!(back.assets[&big].bytes.len(), SMALL as usize + 1000);

        // A new image is appended together with the document.
        let extra = doc.add_asset(vec![9; 500]);
        let third = save(&doc, &path, Some(&second)).unwrap();
        assert_eq!(size(&path), second.end + HEAD + 500 + third.doc);
        let (back, known) = read(&path).unwrap();
        assert_eq!(known.as_ref(), Some(&third));
        assert_eq!(back.assets[&extra].bytes, vec![9; 500]);
        assert_eq!(back.assets[&image].bytes, vec![7; 3000]);

        // A save that was cut short: the version before it is what opens, and the next save
        // continues from there.
        let whole = size(&path);
        File::options().write(true).open(&path).unwrap().set_len(whole - 10).unwrap();
        let (back, known) = read(&path).unwrap();
        assert_eq!(known.as_ref(), Some(&second));
        assert_eq!(back.count(), 3);
        assert!(!back.assets.contains_key(&extra));
        let fourth = save(&doc, &path, known.as_ref()).unwrap();
        assert_eq!(size(&path), fourth.end);
        let (back, _) = read(&path).unwrap();
        assert_eq!(back.assets[&extra].bytes, vec![9; 500]);

        // Without knowledge of the file, or once the file is something else, it is written afresh.
        std::fs::write(&path, b"not this file").unwrap();
        let fifth = save(&doc, &path, Some(&fourth)).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(!fifth.garbage());
        assert_eq!(fifth.images.len(), 3);
    }

    #[test]
    fn old_versions_are_dropped_before_they_pile_up() {
        let mut stored = Stored { end: 100 << 20, doc: 2 << 20, ..Stored::default() };
        assert!(stored.append_ok());
        stored.dead = 24 << 20;
        stored.end += stored.dead;
        assert!(!stored.append_ok());
        // Small files are never appended to.
        assert!(!Stored { end: 1 << 20, doc: 1000, ..Stored::default() }.append_ok());
    }
}
