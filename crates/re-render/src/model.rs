//! What a page holds, in tablet pixels: x 0..1404 from the left, y down from the top.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    /// Stroke width at this point, in pixels.
    pub width: f32,
    /// 0..1.
    pub pressure: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Ballpoint,
    Fineliner,
    Marker,
    Pencil,
    MechanicalPencil,
    Paintbrush,
    Calligraphy,
    Highlighter,
    Shader,
    Eraser,
    EraseArea,
    Other(u32),
}

impl Tool {
    /// Tool ids from both file versions (v6 reuses v5's plus a "v2" range).
    pub fn from_id(id: u32) -> Tool {
        match id {
            2 | 15 => Tool::Ballpoint,
            4 | 17 => Tool::Fineliner,
            3 | 16 => Tool::Marker,
            1 | 14 => Tool::Pencil,
            7 | 13 => Tool::MechanicalPencil,
            0 | 12 => Tool::Paintbrush,
            21 => Tool::Calligraphy,
            5 | 18 => Tool::Highlighter,
            23 => Tool::Shader,
            6 => Tool::Eraser,
            8 => Tool::EraseArea,
            n => Tool::Other(n),
        }
    }

    /// Drawn translucent over what's beneath.
    pub fn translucent(self) -> bool {
        matches!(self, Tool::Highlighter | Tool::Shader)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Black,
    Gray,
    White,
    Yellow,
    Green,
    Pink,
    Blue,
    Red,
    GrayOverlap,
    /// A colour given directly (newer firmware), 0xAARRGGBB.
    Argb(u32),
}

impl Color {
    pub fn from_id(id: u32) -> Color {
        match id {
            0 => Color::Black,
            1 => Color::Gray,
            2 => Color::White,
            3 => Color::Yellow,
            4 => Color::Green,
            5 => Color::Pink,
            6 => Color::Blue,
            7 => Color::Red,
            8 => Color::GrayOverlap,
            9 => Color::Yellow, // highlight
            10 => Color::Green, // green highlight
            11 => Color::Blue,  // cyan highlight
            12 => Color::Pink,  // magenta highlight
            _ => Color::Black,
        }
    }

    /// RGB, 0..1.
    pub fn rgb(self, tool: Tool) -> [f32; 3] {
        let hex = |v: u32| [((v >> 16) & 0xff) as f32 / 255.0, ((v >> 8) & 0xff) as f32 / 255.0, (v & 0xff) as f32 / 255.0];
        match (self, tool) {
            (Color::Argb(v), _) => hex(v),
            (_, Tool::Eraser) => [1.0, 1.0, 1.0],
            // The highlighter is yellow unless it says otherwise.
            (Color::Black | Color::Yellow, Tool::Highlighter) => hex(0xffed75),
            (Color::Black, _) => [0.0, 0.0, 0.0],
            (Color::Gray | Color::GrayOverlap, _) => hex(0x7d7d7d),
            (Color::White, _) => [1.0, 1.0, 1.0],
            (Color::Yellow, _) => hex(0xf5d000),
            (Color::Green, _) => hex(0x4ab43c),
            (Color::Pink, _) => hex(0xe2578f),
            (Color::Blue, _) => hex(0x2f60d0),
            (Color::Red, _) => hex(0xd72f2f),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub tool: Tool,
    pub color: Color,
    pub thickness_scale: f32,
    pub points: Vec<Point>,
}

/// Text highlighted in a PDF/EPUB (v6), with the boxes it covers on the page.
#[derive(Debug, Clone, PartialEq)]
pub struct Highlight {
    pub text: String,
    pub color: Color,
    /// x, y, w, h in page pixels.
    pub rects: Vec<[f32; 4]>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub lines: Vec<Line>,
    pub highlights: Vec<Highlight>,
}

impl Page {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.highlights.is_empty()
    }

    /// Bounding box of everything drawn: (min x, min y, max x, max y).
    pub fn bounds(&self) -> Option<[f32; 4]> {
        let pts = self.lines.iter().flat_map(|l| l.points.iter().map(|p| (p.x, p.y)));
        let rects = self.highlights.iter().flat_map(|h| h.rects.iter().flat_map(|r| [(r[0], r[1]), (r[0] + r[2], r[1] + r[3])]));
        pts.chain(rects).fold(None, |b, (x, y)| {
            Some(match b {
                None => [x, y, x, y],
                Some([a, b, c, d]) => [a.min(x), b.min(y), c.max(x), d.max(y)],
            })
        })
    }
}
