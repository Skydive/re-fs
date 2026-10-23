//! Version 6 `.rm` files (reMarkable OS 3+): a sequence of tagged blocks describing a
//! CRDT scene tree. For drawing we only need the leaves: stroke items (lines) and
//! glyph items (text highlights). Group nesting and typed text are skipped; strokes
//! anchored to typed text are drawn where they are stored.
//!
//! Block header: `len: u32, unknown: u8, min_version: u8, version: u8, type: u8`.
//! Fields inside a block are tagged with a varint `(index << 4) | type`.

use crate::model::{Color, Highlight, Line, Page, Point, Tool};
use crate::reader::{Reader, TagType};
use crate::Error;

const HEADER_LEN: usize = 43;
/// x is stored relative to the page's centre line.
const X_OFFSET: f32 = crate::PAGE_W / 2.0;

const BLOCK_GLYPH_ITEM: u8 = 0x03;
const BLOCK_LINE_ITEM: u8 = 0x05;

pub fn parse(bytes: &[u8]) -> Result<Page, Error> {
    let mut r = Reader::new(&bytes[HEADER_LEN..]);
    let mut page = Page::default();
    while r.remaining() > 0 {
        let start = HEADER_LEN + r.pos;
        let len = r.u32()? as usize;
        let _unknown = r.u8()?;
        let _min_version = r.u8()?;
        let version = r.u8()?;
        let ty = r.u8()?;
        let body = r.bytes(len).map_err(|_| Error::Malformed(start))?;
        let mut b = Reader::new(body);
        // A block we can't read is skipped rather than failing the page.
        let _ = match ty {
            BLOCK_LINE_ITEM => line_item(&mut b, version).map(|l| page.lines.extend(l)),
            BLOCK_GLYPH_ITEM => glyph_item(&mut b).map(|h| page.highlights.extend(h)),
            _ => Ok(()),
        };
    }
    Ok(page)
}

/// The fields every scene item block starts with. Returns the value sub-block, or
/// None for a deleted item (tombstone).
fn item_value<'a>(b: &mut Reader<'a>) -> Result<Option<Reader<'a>>, Error> {
    b.tagged_id(1)?; // parent
    b.tagged_id(2)?; // item id
    b.tagged_id(3)?; // left
    b.tagged_id(4)?; // right
    let deleted = b.tagged_u32(5)?.unwrap_or(0);
    if deleted > 0 {
        return Ok(None);
    }
    b.tagged_subblock(6)
}

fn line_item(b: &mut Reader, version: u8) -> Result<Option<Line>, Error> {
    let Some(mut v) = item_value(b)? else { return Ok(None) };
    let _item_type = v.u8()?; // 3 = line
    let tool = Tool::from_id(v.tagged_u32(1)?.ok_or(Error::Malformed(v.pos))?);
    let mut color = Color::from_id(v.tagged_u32(2)?.unwrap_or(0));
    let thickness_scale = v.tagged_f64(3)?.unwrap_or(1.0) as f32;
    v.tagged_f32(4)?; // starting length
    let mut pts = v.tagged_subblock(5)?.ok_or(Error::Malformed(v.pos))?;
    let mut points = Vec::with_capacity(pts.remaining() / 14);
    while pts.remaining() > 0 {
        let p = if version >= 2 {
            let (x, y) = (pts.f32()?, pts.f32()?);
            let _speed = pts.u16()?;
            let width = f32::from(pts.u16()?) / 4.0;
            let _direction = pts.u8()?;
            let pressure = f32::from(pts.u8()?) / 255.0;
            Point { x: x + X_OFFSET, y, width, pressure }
        } else {
            let (x, y) = (pts.f32()?, pts.f32()?);
            let (_speed, _direction, width, pressure) = (pts.f32()?, pts.f32()?, pts.f32()?, pts.f32()?);
            Point { x: x + X_OFFSET, y, width, pressure }
        };
        points.push(p);
    }
    // Newer firmware: timestamp (6), move id (7), then an explicit ARGB colour (8).
    while v.remaining() > 0 {
        match v.peek_tag() {
            Some((8, t)) if t == TagType::Byte4 as u8 => {
                if let Some(argb) = v.tagged_u32(8)? {
                    color = Color::Argb(argb);
                }
            }
            Some(_) => v.skip_tagged()?,
            None => break,
        }
    }
    Ok(Some(Line { tool, color, thickness_scale, points }))
}

fn glyph_item(b: &mut Reader) -> Result<Option<Highlight>, Error> {
    let Some(mut v) = item_value(b)? else { return Ok(None) };
    let _item_type = v.u8()?; // 1 = glyph range
    v.tagged_u32(2)?; // start
    v.tagged_u32(3)?; // length
    let color = Color::from_id(v.tagged_u32(4)?.unwrap_or(3));
    let text = match v.tagged_subblock(5)? {
        Some(mut s) => {
            let n = s.varuint()? as usize;
            let _is_ascii = s.u8()?;
            String::from_utf8_lossy(s.bytes(n)?).into_owned()
        }
        None => String::new(),
    };
    let mut rects = Vec::new();
    if let Some(mut rs) = v.tagged_subblock(6)? {
        let n = rs.varuint()?;
        for _ in 0..n {
            let (x, y, w, h) = (rs.f64()?, rs.f64()?, rs.f64()?, rs.f64()?);
            rects.push([x as f32 + X_OFFSET, y as f32, w as f32, h as f32]);
        }
    }
    Ok(Some(Highlight { text, color, rects }))
}
