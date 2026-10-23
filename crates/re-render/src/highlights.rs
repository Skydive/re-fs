//! Highlights as Markdown, for notes apps: one section per page that has any.

use crate::model::Page;

/// `pages` is (1-based page number as shown on the tablet, parsed page).
pub fn markdown(title: &str, pages: &[(usize, &Page)]) -> String {
    let mut out = format!("# {title}\n");
    let mut any = false;
    for (n, page) in pages {
        // Highlights in reading order: top to bottom, then left to right.
        let mut hs: Vec<_> = page.highlights.iter().filter(|h| !h.text.trim().is_empty()).collect();
        hs.sort_by(|a, b| {
            let key = |h: &crate::Highlight| h.rects.first().map_or((0.0, 0.0), |r| (r[1], r[0]));
            key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal)
        });
        let drawn = page.lines.iter().any(|l| !l.tool.translucent());
        if hs.is_empty() && !drawn {
            continue;
        }
        any = true;
        out.push_str(&format!("\n## Page {n}\n\n"));
        for h in hs {
            let text = h.text.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push_str(&format!("> {text}\n\n"));
        }
        if drawn {
            out.push_str("*Handwritten notes on this page.*\n");
        }
    }
    if !any {
        out.push_str("\nNo highlights or notes.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Color, Highlight};

    #[test]
    fn sections_in_reading_order() {
        let h = |t: &str, y: f32| Highlight { text: t.into(), color: Color::Yellow, rects: vec![[10.0, y, 5.0, 5.0]] };
        let p = Page { lines: vec![], highlights: vec![h("second  line", 50.0), h("first", 10.0), h("  ", 5.0)] };
        let md = markdown("Paper", &[(3, &p), (4, &Page::default())]);
        assert_eq!(md, "# Paper\n\n## Page 3\n\n> first\n\n> second line\n\n");
        assert!(markdown("X", &[]).contains("No highlights"));
    }
}
