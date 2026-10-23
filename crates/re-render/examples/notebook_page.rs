//! Debug helper: render .rm pages as a notebook PDF.
//! cargo run -p re-render --example notebook_page -- out.pdf page.rm...
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pages: Vec<_> = args[2..].iter().map(|f| Some(re_render::parse(&std::fs::read(f).unwrap()).unwrap())).collect();
    std::fs::write(&args[1], re_render::pdf::notebook(&pages).unwrap()).unwrap();
}
