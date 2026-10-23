//! PDF file attachments (the document catalog's /Names /EmbeddedFiles name tree).
//! re-fs embeds a document's `.rmdoc` in the PDFs it makes, so copying such a
//! PDF back restores the original document with its strokes.

use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};

use crate::Error;

/// The name the `.rmdoc` is attached under.
pub const RMDOC_NAME: &str = "re-fs.rmdoc";
const RMDOC_DESC: &str = "re-fs: the reMarkable document (original file, strokes, page order). Copy this PDF back into a re-fs mount to restore it.";

fn resolve<'a>(doc: &'a Document, o: &'a Object) -> Option<&'a Object> {
    match o {
        Object::Reference(r) => doc.get_object(*r).ok(),
        o => Some(o),
    }
}

/// Attach `data` as `name` (replacing nothing; call once per document).
pub fn attach(doc: &mut Document, name: &str, data: &[u8]) -> Result<(), Error> {
    let ef = Stream::new(
        dictionary! {
            "Type" => "EmbeddedFile",
            "Subtype" => Object::Name(b"application/zip".to_vec()),
            "Params" => dictionary! { "Size" => data.len() as i64 },
        },
        data.to_vec(),
    );
    let ef_id = doc.add_object(ef);
    let spec_id = doc.add_object(dictionary! {
        "Type" => "Filespec",
        "F" => Object::string_literal(name),
        "UF" => Object::string_literal(name),
        "Desc" => Object::string_literal(if name == RMDOC_NAME { RMDOC_DESC } else { "" }),
        "AFRelationship" => "Source",
        "EF" => dictionary! { "F" => ef_id },
    });
    let pair = vec![Object::string_literal(name), Object::Reference(spec_id)];

    let root: ObjectId = doc.trailer.get(b"Root")?.as_reference()?;
    // The catalog's /Names may be a reference or inline; work on a copy, put it back.
    let names: Dictionary = {
        let catalog = doc.get_dictionary(root)?;
        match catalog.get(b"Names").ok().and_then(|o| resolve(doc, o)) {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => Dictionary::new(),
        }
    };
    let mut names = names;
    let tree: Dictionary = match names.get(b"EmbeddedFiles").ok().and_then(|o| resolve(doc, o)) {
        Some(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    };
    let mut tree = tree;
    if let Ok(kids) = tree.get(b"Kids").and_then(Object::as_array).cloned() {
        // An existing tree with child nodes: add ours as one more leaf.
        let leaf = doc.add_object(dictionary! {
            "Names" => pair,
            "Limits" => vec![Object::string_literal(name), Object::string_literal(name)],
        });
        let mut kids = kids;
        kids.push(Object::Reference(leaf));
        tree.set("Kids", kids);
    } else {
        // A flat tree: keys must stay sorted.
        let mut flat = tree.get(b"Names").and_then(Object::as_array).cloned().unwrap_or_default();
        let at = flat.chunks(2).position(|kv| kv[0].as_str().is_ok_and(|k| k > name.as_bytes())).map_or(flat.len(), |i| i * 2);
        flat.splice(at..at, pair);
        tree.set("Names", flat);
        tree.remove(b"Limits");
    }
    names.set("EmbeddedFiles", tree);
    doc.get_dictionary_mut(root)?.set("Names", names);
    Ok(())
}

/// The attachment called `name`, if the PDF has one.
pub fn extract(pdf: &[u8], name: &str) -> Option<Vec<u8>> {
    let doc = crate::pdf::load_tolerant(pdf).ok()?;
    let root = doc.trailer.get(b"Root").ok()?.as_reference().ok()?;
    let catalog = doc.get_dictionary(root).ok()?;
    let names = resolve(&doc, catalog.get(b"Names").ok()?)?.as_dict().ok()?;
    let tree = resolve(&doc, names.get(b"EmbeddedFiles").ok()?)?.as_dict().ok()?;
    let spec = find(&doc, tree, name.as_bytes(), 0)?;
    let spec = resolve(&doc, spec)?.as_dict().ok()?;
    let ef = resolve(&doc, spec.get(b"EF").ok()?)?.as_dict().ok()?;
    let stream = resolve(&doc, ef.get(b"F").ok()?)?.as_stream().ok()?;
    if stream.dict.has(b"Filter") {
        stream.decompressed_content().ok()
    } else {
        Some(stream.content.clone())
    }
}

/// Look `key` up in a name tree node (and its kids).
fn find<'a>(doc: &'a Document, node: &'a Dictionary, key: &[u8], depth: u32) -> Option<&'a Object> {
    if depth > 16 {
        return None;
    }
    if let Ok(flat) = node.get(b"Names").and_then(Object::as_array) {
        for kv in flat.chunks(2) {
            if kv.len() == 2 && kv[0].as_str().is_ok_and(|k| k == key) {
                return Some(&kv[1]);
            }
        }
    }
    if let Ok(kids) = node.get(b"Kids").and_then(Object::as_array) {
        for kid in kids {
            if let Some(d) = resolve(doc, kid).and_then(|o| o.as_dict().ok()) {
                if let Some(v) = find(doc, d, key, depth + 1) {
                    return Some(v);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf;

    #[test]
    fn round_trips_through_a_saved_compressed_pdf() {
        let data: Vec<u8> = (0..50_000u32).map(|i| (i % 253) as u8).collect();
        let out = pdf::notebook_with(&[None], Some(&data)).unwrap();
        assert_eq!(extract(&out, RMDOC_NAME).as_deref(), Some(&data[..]));
        assert_eq!(extract(&out, "other"), None);
        // Still a normal one-page PDF.
        assert_eq!(Document::load_mem(&out).unwrap().get_pages().len(), 1);
    }

    #[test]
    fn none_without_an_attachment() {
        let plain = pdf::notebook(&[None]).unwrap();
        assert_eq!(extract(&plain, RMDOC_NAME), None);
        assert_eq!(extract(b"not a pdf", RMDOC_NAME), None);
    }

    #[test]
    fn keeps_existing_attachments() {
        // A PDF that already has an attachment keeps it, and gains ours.
        let mut doc = Document::load_mem(&pdf::notebook(&[None]).unwrap()).unwrap();
        attach(&mut doc, "zz-their-file.txt", b"theirs").unwrap();
        attach(&mut doc, RMDOC_NAME, b"ours").unwrap();
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        assert_eq!(extract(&out, "zz-their-file.txt").as_deref(), Some(&b"theirs"[..]));
        assert_eq!(extract(&out, RMDOC_NAME).as_deref(), Some(&b"ours"[..]));
    }
}
