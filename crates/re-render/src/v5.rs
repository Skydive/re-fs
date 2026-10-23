//! Version 3 and 5 `.rm` files (reMarkable OS 2): a fixed layout.
//!
//! ```text
//! header (43 bytes) | layers: u32
//!   per layer: lines: u32
//!     per line: tool u32, color u32, padding u32, thickness f32, [v5: unknown u32], points u32
//!       per point: x f32, y f32, speed f32, direction f32, width f32, pressure f32
//! ```
//! Coordinates are page pixels with x from the left edge.

use crate::model::{Color, Line, Page, Point, Tool};
use crate::reader::Reader;
use crate::Error;

pub fn parse(bytes: &[u8]) -> Result<Page, Error> {
    let v5 = bytes[32] == b'5';
    let mut r = Reader::new(&bytes[43..]);
    let mut page = Page::default();
    let layers = r.u32()?;
    for _ in 0..layers {
        let lines = r.u32()?;
        for _ in 0..lines {
            let tool = Tool::from_id(r.u32()?);
            let color = Color::from_id(r.u32()?);
            let _padding = r.u32()?;
            let thickness_scale = r.f32()?;
            if v5 {
                let _unknown = r.u32()?;
            }
            let n = r.u32()? as usize;
            if n > r.remaining() / 24 {
                return Err(Error::Malformed(r.pos));
            }
            let mut points = Vec::with_capacity(n);
            for _ in 0..n {
                let (x, y, _speed, _direction, width, pressure) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?, r.f32()?, r.f32()?);
                points.push(Point { x, y, width, pressure });
            }
            page.lines.push(Line { tool, color, thickness_scale, points });
        }
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A v5 file with one layer and one two-point ballpoint line.
    pub fn sample() -> Vec<u8> {
        let mut b = b"reMarkable .lines file, version=5          ".to_vec();
        let u = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        let f = |b: &mut Vec<u8>, v: f32| b.extend_from_slice(&v.to_le_bytes());
        u(&mut b, 1); // layers
        u(&mut b, 1); // lines
        u(&mut b, 2); // ballpoint
        u(&mut b, 0); // black
        u(&mut b, 0);
        f(&mut b, 2.0);
        u(&mut b, 0);
        u(&mut b, 2); // points
        for (x, y) in [(100.0, 200.0), (300.0, 400.0)] {
            for v in [x, y, 0.1, 0.2, 3.0, 0.5] {
                f(&mut b, v);
            }
        }
        b
    }

    #[test]
    fn parses_v5() {
        let page = crate::parse(&sample()).unwrap();
        assert_eq!(page.lines.len(), 1);
        let l = &page.lines[0];
        assert_eq!((l.tool, l.color, l.thickness_scale), (Tool::Ballpoint, Color::Black, 2.0));
        assert_eq!(l.points[1], Point { x: 300.0, y: 400.0, width: 3.0, pressure: 0.5 });
    }

    #[test]
    fn rejects_truncated() {
        let mut s = sample();
        s.truncate(s.len() - 10);
        assert!(crate::parse(&s).is_err());
    }
}
