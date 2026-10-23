//! Debug helper: try loading a PDF with each repair, then annotate it.
//! cargo run -p re-render --example annotate_file -- in.pdf out.pdf
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let src = std::fs::read(&args[1]).unwrap();
    let k = src.windows(5).position(|w| w == b"%PDF-").unwrap_or(0);
    let stripped = &src[k..];
    for (name, bytes) in [
        ("as is", src.clone()),
        ("stripped", stripped.to_vec()),
        ("stripped+eof", [stripped, b"\n%%EOF\n"].concat()),
    ] {
        match lopdf::Document::load_mem(&bytes) {
            Ok(d) => eprintln!("{name}: ok, {} pages", d.get_pages().len()),
            Err(e) => eprintln!("{name}: {e:?}"),
        }
    }
    match re_render::pdf::annotate(&src, &[re_render::pdf::PageSpec { source: Some(0), strokes: None }]) {
        Ok(out) => {
            std::fs::write(&args[2], out).unwrap();
            eprintln!("annotate: ok");
        }
        Err(e) => eprintln!("annotate: {e}"),
    }
}
