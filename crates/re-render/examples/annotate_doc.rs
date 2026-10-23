//! Debug helper: annotate a PDF with explicit pages.
//! cargo run -p re-render --example annotate_doc -- in.pdf out.pdf SRC:file.rm|-:file.rm|SRC ...
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let src = std::fs::read(&args[1]).unwrap();
    let specs: Vec<_> = args[3..]
        .iter()
        .map(|a| {
            let (s, f) = a.split_once(':').map_or((a.as_str(), None), |(s, f)| (s, Some(f)));
            re_render::pdf::PageSpec {
                source: s.parse().ok(),
                strokes: f.map(|f| re_render::parse(&std::fs::read(f).unwrap()).unwrap()),
            }
        })
        .collect();
    std::fs::write(&args[2], re_render::pdf::annotate(&src, &specs).unwrap()).unwrap();
}
