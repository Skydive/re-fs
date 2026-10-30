use re_render::model::{Color, Tool};
use re_render::pdf::{self, PageSpec};

const SAMPLE: &[u8] = include_bytes!("data/sample-v6.rm");

#[test]
fn parses_sample_v6() {
    let page = re_render::parse(SAMPLE).unwrap();
    assert!(!page.lines.is_empty());
    let first = &page.lines[0];
    assert_eq!((first.tool, first.color), (Tool::Ballpoint, Color::Black));
    assert_eq!(first.points.len(), 41);
    // "a handwritten line" sits near the top left of the page.
    let [x0, y0, x1, y1] = page.bounds().unwrap();
    assert!(x0 > 100.0 && x1 < 1100.0 && y0 > 150.0 && y1 < 450.0, "bounds {x0} {y0} {x1} {y1}");
    eprintln!("sample: {} lines, bounds {:?}", page.lines.len(), page.bounds());
}

#[test]
fn notebook_pdf_loads_back() {
    let page = re_render::parse(SAMPLE).unwrap();
    let bytes = pdf::notebook(&[Some(page), None]).unwrap();
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    assert_eq!(doc.get_pages().len(), 2);
    if let Ok(dir) = std::env::var("RE_RENDER_OUT") {
        std::fs::write(format!("{dir}/sample.pdf"), &bytes).unwrap();
    }
}

#[test]
fn annotating_keeps_contents_held_by_reference_to_an_array() {
    // /Contents 9 0 R, where 9 0 obj is [ stream stream ]: the page's text must stay.
    use lopdf::{Object, Stream};
    let mut src = lopdf::Document::load_mem(&pdf::notebook(&[None]).unwrap()).unwrap();
    let page = src.get_pages()[&1];
    let a = src.add_object(Stream::new(lopdf::dictionary! {}, b"% first\n".to_vec()));
    let b = src.add_object(Stream::new(lopdf::dictionary! {}, b"% second\n".to_vec()));
    let array = src.add_object(vec![Object::Reference(a), Object::Reference(b)]);
    src.get_object_mut(page).unwrap().as_dict_mut().unwrap().set("Contents", array);
    let mut bytes = Vec::new();
    src.save_to(&mut bytes).unwrap();

    let strokes = re_render::parse(SAMPLE).unwrap();
    let out = pdf::annotate(&bytes, &[PageSpec { source: Some(0), strokes: Some(strokes) }]).unwrap();

    let doc = lopdf::Document::load_mem(&out).unwrap();
    let page = doc.get_dictionary(doc.get_pages()[&1]).unwrap();
    let contents = page.get(b"Contents").unwrap().as_array().unwrap();
    let streams: Vec<&Stream> = contents
        .iter()
        .map(|o| doc.get_object(o.as_reference().unwrap()).unwrap().as_stream().expect("every /Contents item is a stream"))
        .collect();
    assert!(streams.iter().any(|s| s.content.starts_with(b"% first")));
    assert!(streams.iter().any(|s| s.content.starts_with(b"% second")));
}

#[test]
fn annotates_an_existing_pdf() {
    // A two-page source PDF, built with the notebook writer.
    let src = pdf::notebook(&[None, None]).unwrap();
    let strokes = re_render::parse(SAMPLE).unwrap();
    let specs = [
        PageSpec { source: Some(1), strokes: None },
        PageSpec { source: None, strokes: Some(strokes.clone()) }, // inserted page
        PageSpec { source: Some(0), strokes: Some(strokes) },
    ];
    let out = pdf::annotate(&src, &specs).unwrap();
    let doc = lopdf::Document::load_mem(&out).unwrap();
    assert_eq!(doc.get_pages().len(), 3);
    if let Ok(dir) = std::env::var("RE_RENDER_OUT") {
        std::fs::write(format!("{dir}/annotated.pdf"), &out).unwrap();
    }
}
