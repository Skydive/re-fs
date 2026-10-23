//! PDF output: notebooks as new PDFs, and annotated PDFs (the original pages with
//! strokes and highlights drawn on top, in the tablet's page order).

use std::collections::BTreeMap;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};

use crate::model::{Line, Page, Tool};
use crate::{Error, PAGE_H, PAGE_W};

/// Points per tablet pixel (226 dpi).
pub const PT_PER_PX: f32 = 72.0 / 226.0;
const GS_TRANSLUCENT: &str = "ReFsHl";

/// How tablet pixels map onto a PDF page: `pdf = (x_px * a + c, y_px * d + f)`.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub a: f32,
    pub d: f32,
    pub c: f32,
    pub f: f32,
}

impl Placement {
    /// A tablet page at 226 dpi covering `bx` = [x0, y0, x1, y1] in tablet pixels
    /// (the screen, grown to hold strokes drawn beyond it). Returns the placement
    /// and the page size in points.
    pub fn canvas(bx: [f32; 4]) -> (Self, f32, f32) {
        let [x0, y0, x1, y1] = bx;
        let s = PT_PER_PX;
        (Placement { a: s, d: -s, c: -x0 * s, f: y1 * s }, (x1 - x0) * s, (y1 - y0) * s)
    }

    /// Strokes on a PDF page of `w`×`h` points (origin `x0`, `y0`). The tablet
    /// stores them at its physical resolution (226 dpi), x relative to the page's
    /// centre line and y down from its top, whatever the zoom they were drawn at.
    /// (Checked against xochitl's own thumbnails of annotated A4 pages.)
    pub fn pdf_page(x0: f32, y0: f32, w: f32, h: f32) -> Self {
        let s = PT_PER_PX;
        Placement { a: s, d: -s, c: x0 + w / 2.0 - (PAGE_W / 2.0) * s, f: y0 + h }
    }

    fn cm(&self) -> Operation {
        Operation::new("cm", vec![self.a.into(), 0.into(), 0.into(), self.d.into(), self.c.into(), self.f.into()])
    }
}

/// Drawing operations for a page's strokes and highlights, in tablet pixels
/// (wrap them with `Placement::cm`).
fn page_ops(page: &Page) -> Vec<Operation> {
    let mut ops = vec![Operation::new("J", vec![1.into()]), Operation::new("j", vec![1.into()])];
    // Highlights under the strokes.
    for h in &page.highlights {
        let [r, g, b] = h.color.rgb(Tool::Highlighter);
        ops.push(Operation::new("q", vec![]));
        ops.push(Operation::new("gs", vec![Object::Name(GS_TRANSLUCENT.into())]));
        ops.push(Operation::new("rg", vec![r.into(), g.into(), b.into()]));
        for [x, y, w, hh] in &h.rects {
            ops.push(Operation::new("re", vec![(*x).into(), (*y).into(), (*w).into(), (*hh).into()]));
        }
        ops.push(Operation::new("f", vec![]));
        ops.push(Operation::new("Q", vec![]));
    }
    for line in &page.lines {
        if matches!(line.tool, Tool::EraseArea) || line.points.is_empty() {
            continue;
        }
        line_ops(line, &mut ops);
    }
    ops
}

/// One stroke, split into runs of similar width (pens vary width with pressure).
fn line_ops(line: &Line, ops: &mut Vec<Operation>) {
    let [r, g, b] = line.color.rgb(line.tool);
    ops.push(Operation::new("q", vec![]));
    if line.tool.translucent() {
        ops.push(Operation::new("gs", vec![Object::Name(GS_TRANSLUCENT.into())]));
        ops.push(Operation::new("J", vec![2.into()])); // highlighters are square-ended
    }
    ops.push(Operation::new("RG", vec![r.into(), g.into(), b.into()]));
    let width = |w: f32| match line.tool {
        Tool::Pencil | Tool::MechanicalPencil => (w * 0.8).max(0.8),
        _ => w.max(1.0),
    };
    let pts = &line.points;
    if pts.len() == 1 {
        let p = pts[0];
        ops.push(Operation::new("w", vec![width(p.width).into()]));
        ops.push(Operation::new("m", vec![p.x.into(), p.y.into()]));
        ops.push(Operation::new("l", vec![(p.x + 0.01).into(), p.y.into()]));
        ops.push(Operation::new("S", vec![]));
    }
    let mut i = 0;
    while i + 1 < pts.len() {
        let w0 = pts[i].width;
        let mut j = i + 1;
        while j + 1 < pts.len() && (pts[j].width - w0).abs() <= w0.max(1.0) * 0.15 {
            j += 1;
        }
        let avg = pts[i..=j].iter().map(|p| p.width).sum::<f32>() / (j - i + 1) as f32;
        ops.push(Operation::new("w", vec![width(avg).into()]));
        ops.push(Operation::new("m", vec![pts[i].x.into(), pts[i].y.into()]));
        for p in &pts[i + 1..=j] {
            ops.push(Operation::new("l", vec![p.x.into(), p.y.into()]));
        }
        ops.push(Operation::new("S", vec![]));
        i = j;
    }
    ops.push(Operation::new("Q", vec![]));
}

fn ext_gstate() -> Dictionary {
    dictionary! { GS_TRANSLUCENT => dictionary! { "Type" => "ExtGState", "CA" => 0.45, "ca" => 0.45, "BM" => "Multiply" } }
}

fn overlay(page: &Page, at: Placement) -> Result<Vec<u8>, Error> {
    let mut ops = vec![Operation::new("q", vec![]), at.cm()];
    ops.extend(page_ops(page));
    ops.push(Operation::new("Q", vec![]));
    Ok(Content { operations: ops }.encode()?)
}

fn save(mut doc: Document) -> Result<Vec<u8>, Error> {
    doc.compress();
    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(lopdf::Error::IO)?;
    Ok(out)
}

/// The screen, grown to include everything drawn on `page` (notebook pages are
/// an open canvas: strokes can go above, below and beside the screen).
fn canvas_box(page: &Page) -> [f32; 4] {
    let screen = [0.0, 0.0, PAGE_W, PAGE_H];
    match page.bounds() {
        // A little margin so stroke widths at the edge aren't clipped.
        Some([a, b, c, d]) => [screen[0].min(a - 10.0), screen[1].min(b - 10.0), screen[2].max(c + 10.0), screen[3].max(d + 10.0)],
        None => screen,
    }
}

/// A notebook: one page per entry (`None` = a blank page), each at least
/// screen-sized and grown to hold all its strokes.
pub fn notebook(pages: &[Option<Page>]) -> Result<Vec<u8>, Error> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let res = doc.add_object(dictionary! { "ExtGState" => ext_gstate() });
    let mut kids = Vec::new();
    for p in pages {
        let page = p.as_ref().cloned().unwrap_or_default();
        let (at, w, h) = Placement::canvas(canvas_box(&page));
        let content = overlay(&page, at)?;
        let cid = doc.add_object(Stream::new(dictionary! {}, content));
        let pid = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Resources" => res, "Contents" => cid,
            "MediaBox" => vec![0.into(), 0.into(), w.into(), h.into()],
        });
        kids.push(Object::Reference(pid));
    }
    let count = kids.len() as i64;
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    save(doc)
}

/// One page of an annotated document, in the tablet's order.
#[derive(Debug, Clone)]
pub struct PageSpec {
    /// The original page shown (0-based), or None for a blank page inserted on the tablet.
    pub source: Option<u32>,
    pub strokes: Option<Page>,
}

/// An inheritable page attribute, looked up through the page's parents.
fn inherited(doc: &Document, page: ObjectId, key: &[u8]) -> Option<Object> {
    let mut id = page;
    for _ in 0..32 {
        let dict = doc.get_dictionary(id).ok()?;
        if let Ok(v) = dict.get(key) {
            return Some(v.clone());
        }
        id = dict.get(b"Parent").and_then(Object::as_reference).ok()?;
    }
    None
}

fn number(o: &Object) -> f32 {
    match o {
        Object::Integer(i) => *i as f32,
        Object::Real(r) => *r,
        _ => 0.0,
    }
}

fn page_box(doc: &Document, page: ObjectId) -> [f32; 4] {
    let b = inherited(doc, page, b"CropBox").or_else(|| inherited(doc, page, b"MediaBox"));
    let arr = b.and_then(|o| match o {
        Object::Reference(r) => doc.get_object(r).ok().cloned(),
        o => Some(o),
    });
    match arr {
        Some(Object::Array(a)) if a.len() == 4 => {
            let v: Vec<f32> = a.iter().map(number).collect();
            [v[0].min(v[2]), v[1].min(v[3]), (v[2] - v[0]).abs(), (v[3] - v[1]).abs()]
        }
        _ => [0.0, 0.0, 612.0, 792.0],
    }
}

/// Ways real-world PDFs are slightly broken that viewers repair silently, as
/// candidates to try in order: as is; junk (often a UTF-8 BOM) before the header
/// removed, for files whose offsets don't count it; the header moved to byte 0
/// with the gap filled by a comment, for files whose offsets do; and each of
/// those with a missing final `%%EOF` added.
fn repairs(pdf: &[u8]) -> Vec<std::borrow::Cow<'_, [u8]>> {
    use std::borrow::Cow;
    const H: &[u8] = b"%PDF-";
    let mut out: Vec<Cow<[u8]>> = vec![pdf.into()];
    if let Some(k) = pdf[..pdf.len().min(1024)].windows(H.len()).position(|w| w == H).filter(|&k| k > 0 && pdf.len() >= k + 8) {
        out.push(pdf[k..].into());
        let mut moved = pdf.to_vec();
        moved.copy_within(k..k + 8, 0);
        moved[8] = b'\n';
        moved[9..k + 8].fill(b'%');
        out.push(moved.into());
    }
    let tail = &pdf[pdf.len().saturating_sub(1024)..];
    if !tail.windows(5).any(|w| w == b"%%EOF") {
        let with_eof: Vec<Cow<[u8]>> = out.iter().map(|c| with_eof(c).into()).collect();
        out.extend(with_eof);
    }
    out
}

/// Complete a truncated final `%%EOF` (or add one) without changing the line
/// structure before it: lopdf reads the last lines strictly.
fn with_eof(pdf: &[u8]) -> Vec<u8> {
    let mut end = pdf.len();
    while end > 0 && pdf[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let body = &pdf[..end];
    for partial in [&b"%%EO"[..], b"%%E", b"%%", b"%"] {
        if body.ends_with(partial) {
            return [&body[..body.len() - partial.len()], b"%%EOF\n"].concat();
        }
    }
    [body, b"\n%%EOF\n"].concat()
}

/// The last resort viewers use: find every `N G obj` in the file and append a fresh
/// cross-reference table and trailer pointing at them (keeping /Root and /Info
/// from the old trailer). Works for files with plain objects (no xref streams).
fn rebuild_xref(pdf: &[u8]) -> Option<Vec<u8>> {
    let mut offsets: std::collections::BTreeMap<u32, (usize, u16)> = Default::default();
    let mut i = 0;
    while i < pdf.len() {
        let line_start = i == 0 || matches!(pdf[i - 1], b'\n' | b'\r');
        if line_start && pdf[i].is_ascii_digit() {
            // "<num> <gen> obj"
            let rest = &pdf[i..pdf.len().min(i + 40)];
            let text = String::from_utf8_lossy(rest);
            let mut parts = text.split_ascii_whitespace();
            if let (Some(n), Some(g), Some(kw)) = (parts.next(), parts.next(), parts.next()) {
                if kw.starts_with("obj") {
                    if let (Ok(n), Ok(g)) = (n.parse::<u32>(), g.parse::<u16>()) {
                        offsets.insert(n, (i, g));
                    }
                }
            }
        }
        i += 1;
    }
    let trailer_at = pdf.windows(7).rposition(|w| w == b"trailer")?;
    let tail = String::from_utf8_lossy(&pdf[trailer_at..]);
    let reference = |key: &str| -> Option<String> {
        let at = tail.find(key)? + key.len();
        let mut p = tail[at..].split_ascii_whitespace();
        let (n, g, r) = (p.next()?, p.next()?, p.next()?);
        r.starts_with('R').then(|| format!("{key} {n} {g} R"))
    };
    let root = reference("/Root")?;
    let info = reference("/Info").unwrap_or_default();
    let size = offsets.keys().next_back().map_or(1, |n| n + 1);
    let mut out = pdf.to_vec();
    out.extend_from_slice(b"\n");
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offsets.get(&n) {
            Some((off, g)) => out.extend_from_slice(format!("{off:010} {g:05} n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} {root} {info} >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    Some(out)
}

/// Load a PDF, repairing what viewers would.
fn load_tolerant(pdf: &[u8]) -> Result<Document, Error> {
    let mut first_err = None;
    let mut candidates = repairs(pdf);
    // Last resort: rebuilt cross-reference tables, on the file with its header at 0.
    let base: Vec<u8> = candidates.get(1).map(|c| c.to_vec()).unwrap_or_else(|| pdf.to_vec());
    if let Some(rebuilt) = rebuild_xref(&base) {
        candidates.push(rebuilt.into());
    }
    for candidate in candidates {
        match Document::load_mem(&candidate) {
            Ok(d) => return Ok(d),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    Err(first_err.expect("at least one candidate").into())
}

/// The original PDF with strokes drawn on top, pages arranged as on the tablet.
pub fn annotate(original: &[u8], specs: &[PageSpec]) -> Result<Vec<u8>, Error> {
    let mut doc = load_tolerant(original)?;
    let pages: BTreeMap<u32, ObjectId> = doc.get_pages();
    let root_pages = doc.catalog()?.get(b"Pages")?.as_reference()?;
    let gs_id = doc.add_object(ext_gstate());
    let mut kids = Vec::new();
    let mut used = std::collections::HashSet::new();

    for spec in specs {
        let src = spec.source.and_then(|i| pages.get(&(i + 1)).copied());
        let Some(src) = src else {
            // A page inserted on the tablet: a notebook-style canvas.
            let page = spec.strokes.clone().unwrap_or_default();
            let (at, w, h) = Placement::canvas(canvas_box(&page));
            let content = overlay(&page, at)?;
            let cid = doc.add_object(Stream::new(dictionary! {}, content));
            let pid = doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => root_pages, "Contents" => cid,
                "MediaBox" => vec![0.into(), 0.into(), w.into(), h.into()],
                "Resources" => dictionary! { "ExtGState" => dictionary! { GS_TRANSLUCENT => gs_id } },
            });
            kids.push(Object::Reference(pid));
            continue;
        };
        let bx = page_box(&doc, src);
        // The same original page shown twice gets its own copy.
        let pid = if used.insert(src) {
            src
        } else {
            let copy = doc.get_object(src)?.clone();
            doc.add_object(copy)
        };
        // Pages move out of nested page trees: copy what they inherited.
        let attrs: Vec<(&[u8], Option<Object>)> =
            [b"Resources".as_slice(), b"MediaBox", b"CropBox", b"Rotate"].into_iter().map(|k| (k, inherited(&doc, src, k))).collect();
        let strokes = spec.strokes.as_ref().filter(|p| !p.is_empty());
        let overlay_id = match strokes {
            Some(p) => Some(doc.add_object(Stream::new(dictionary! {}, overlay(p, Placement::pdf_page(bx[0], bx[1], bx[2], bx[3]))?))),
            None => None,
        };
        let q_id = doc.add_object(Stream::new(dictionary! {}, b"q\n".to_vec()));
        let bigq_id = doc.add_object(Stream::new(dictionary! {}, b"\nQ\n".to_vec()));
        let mut resources = match attrs[0].1.clone() {
            Some(Object::Reference(r)) => doc.get_dictionary(r).cloned().unwrap_or_default(),
            Some(Object::Dictionary(d)) => d,
            _ => Dictionary::new(),
        };
        if strokes.is_some() {
            let mut gs = match resources.get(b"ExtGState").cloned() {
                Ok(Object::Reference(r)) => doc.get_dictionary(r).cloned().unwrap_or_default(),
                Ok(Object::Dictionary(d)) => d,
                _ => Dictionary::new(),
            };
            gs.set(GS_TRANSLUCENT, gs_id);
            resources.set("ExtGState", gs);
        }
        let page = doc.get_object_mut(pid)?.as_dict_mut()?;
        for (k, v) in &attrs[1..] {
            if let Some(v) = v {
                page.set(*k, v.clone());
            }
        }
        page.set("Resources", resources);
        page.set("Parent", root_pages);
        if let Some(oid) = overlay_id {
            // Original content inside q/Q so its graphics state can't leak into ours.
            let mut contents = vec![Object::Reference(q_id)];
            match page.get(b"Contents").cloned() {
                Ok(Object::Array(a)) => contents.extend(a),
                Ok(o @ Object::Reference(_)) => contents.push(o),
                _ => {}
            }
            contents.push(Object::Reference(bigq_id));
            contents.push(Object::Reference(oid));
            page.set("Contents", contents);
        }
        kids.push(Object::Reference(pid));
    }

    let count = kids.len() as i64;
    let root = doc.get_object_mut(root_pages)?.as_dict_mut()?;
    root.set("Kids", kids);
    root.set("Count", count);
    doc.prune_objects();
    save(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_before_header() {
        // A minimal one-page PDF whose byte offsets count a UTF-8 BOM in front,
        // as in files saved by tools that add one.
        let mut bom: Vec<u8> = vec![0xef, 0xbb, 0xbf];
        bom.extend_from_slice(b"%PDF-1.4\n");
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>",
        ];
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(bom.len());
            bom.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = bom.len();
        bom.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for o in &offsets {
            bom.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        bom.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
        assert!(Document::load_mem(&bom).is_err(), "lopdf alone rejects it");
        let out = annotate(&bom, &[PageSpec { source: Some(0), strokes: None }]).unwrap();
        assert_eq!(Document::load_mem(&out).unwrap().get_pages().len(), 1);
    }

    #[test]
    fn bom_added_afterwards_and_no_eof() {
        // Offsets that don't count the BOM (it was added later), and no %%EOF.
        let src = notebook(&[None]).unwrap();
        let end = src.windows(5).rposition(|w| w == b"%%EOF").unwrap();
        let mut bad = vec![0xef, 0xbb, 0xbf];
        bad.extend_from_slice(&src[..end]);
        assert!(Document::load_mem(&bad).is_err());
        let out = annotate(&bad, &[PageSpec { source: Some(0), strokes: None }]).unwrap();
        assert_eq!(Document::load_mem(&out).unwrap().get_pages().len(), 1);
    }
}
