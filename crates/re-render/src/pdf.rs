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
    /// A tablet-sized page (notebooks, inserted blank pages).
    pub fn tablet_page() -> Self {
        let h = PAGE_H * PT_PER_PX;
        Placement { a: PT_PER_PX, d: -PT_PER_PX, c: 0.0, f: h }
    }

    /// A PDF page of `w`×`h` points as the tablet shows it: scaled to fit the
    /// screen, centred horizontally, top-aligned.
    pub fn pdf_page(x0: f32, y0: f32, w: f32, h: f32) -> Self {
        let px_per_pt = (PAGE_W / w).min(PAGE_H / h);
        let s = 1.0 / px_per_pt;
        let shown_w = w * px_per_pt;
        let left_px = (PAGE_W - shown_w) / 2.0;
        Placement { a: s, d: -s, c: x0 - left_px * s, f: y0 + h }
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

/// A notebook: one tablet-sized page per entry (`None` = a blank page).
pub fn notebook(pages: &[Option<Page>]) -> Result<Vec<u8>, Error> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let res = doc.add_object(dictionary! { "ExtGState" => ext_gstate() });
    let (w, h) = (PAGE_W * PT_PER_PX, PAGE_H * PT_PER_PX);
    let mut kids = Vec::new();
    for p in pages {
        let content = overlay(p.as_ref().unwrap_or(&Page::default()), Placement::tablet_page())?;
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

/// The original PDF with strokes drawn on top, pages arranged as on the tablet.
pub fn annotate(original: &[u8], specs: &[PageSpec]) -> Result<Vec<u8>, Error> {
    let mut doc = Document::load_mem(original)?;
    let pages: BTreeMap<u32, ObjectId> = doc.get_pages();
    let root_pages = doc.catalog()?.get(b"Pages")?.as_reference()?;
    let gs_id = doc.add_object(ext_gstate());
    let mut kids = Vec::new();
    let mut used = std::collections::HashSet::new();
    let mut last_box = [0.0, 0.0, PAGE_W * PT_PER_PX, PAGE_H * PT_PER_PX];

    for spec in specs {
        let src = spec.source.and_then(|i| pages.get(&(i + 1)).copied());
        let Some(src) = src else {
            // A blank page inserted on the tablet: same size as the page before it.
            let [_, _, w, h] = last_box;
            let at = Placement::pdf_page(0.0, 0.0, w, h);
            let content = overlay(spec.strokes.as_ref().unwrap_or(&Page::default()), at)?;
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
        last_box = bx;
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
