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
