//! Debug helper: summarise a .rm page. cargo run -p re-render --example dump_rm -- page.rm
fn main() {
    for f in std::env::args().skip(1) {
        let page = re_render::parse(&std::fs::read(&f).unwrap()).unwrap();
        let mut tools = std::collections::BTreeMap::<String, usize>::new();
        for l in &page.lines {
            *tools.entry(format!("{:?}/{:?}", l.tool, l.color)).or_default() += 1;
        }
        println!("{f}: {} lines {tools:?}, {} highlights, bounds {:?}", page.lines.len(), page.highlights.len(), page.bounds());
        for h in page.highlights.iter().take(3) {
            println!("  highlight {:?} {:?}", h.text, h.rects.first());
        }
    }
}
