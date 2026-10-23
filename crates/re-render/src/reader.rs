//! A little-endian cursor over a byte slice, with v6's varints and tagged fields.

use crate::Error;

#[derive(Clone)]
pub struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

/// The value-type nibble of a v6 tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagType {
    Id = 0xF,
    Length4 = 0xC,
    Byte8 = 0x8,
    Byte4 = 0x4,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.remaining() < n {
            return Err(Error::Malformed(self.pos));
        }
        let b = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(b)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    pub fn f32(&mut self) -> Result<f32, Error> {
        Ok(f32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    pub fn f64(&mut self) -> Result<f64, Error> {
        Ok(f64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }

    pub fn varuint(&mut self) -> Result<u64, Error> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(Error::Malformed(self.pos))
    }

    /// The next tag, without consuming it: (index, type).
    pub fn peek_tag(&self) -> Option<(u64, u8)> {
        let mut r = self.clone();
        let t = r.varuint().ok()?;
        Some((t >> 4, (t & 0xf) as u8))
    }

    /// Consume the tag if it's (index, type); false (and nothing consumed) if not.
    pub fn take_tag(&mut self, index: u64, ty: TagType) -> bool {
        if self.peek_tag() == Some((index, ty as u8)) {
            let _ = self.varuint();
            true
        } else {
            false
        }
    }

    pub fn tagged_u32(&mut self, index: u64) -> Result<Option<u32>, Error> {
        if self.take_tag(index, TagType::Byte4) { self.u32().map(Some) } else { Ok(None) }
    }

    pub fn tagged_f32(&mut self, index: u64) -> Result<Option<f32>, Error> {
        if self.take_tag(index, TagType::Byte4) { self.f32().map(Some) } else { Ok(None) }
    }

    pub fn tagged_f64(&mut self, index: u64) -> Result<Option<f64>, Error> {
        if self.take_tag(index, TagType::Byte8) { self.f64().map(Some) } else { Ok(None) }
    }

    /// A CRDT id: (u8, varuint).
    pub fn tagged_id(&mut self, index: u64) -> Result<Option<(u8, u64)>, Error> {
        if self.take_tag(index, TagType::Id) { Ok(Some((self.u8()?, self.varuint()?))) } else { Ok(None) }
    }

    /// A length-prefixed sub-block; returns a reader over just its contents.
    pub fn tagged_subblock(&mut self, index: u64) -> Result<Option<Reader<'a>>, Error> {
        if self.take_tag(index, TagType::Length4) {
            let n = self.u32()? as usize;
            Ok(Some(Reader::new(self.bytes(n)?)))
        } else {
            Ok(None)
        }
    }

    /// Skip one tagged value of any type.
    pub fn skip_tagged(&mut self) -> Result<(), Error> {
        let t = self.varuint()?;
        match (t & 0xf) as u8 {
            0xF => {
                self.u8()?;
                self.varuint()?;
            }
            0xC => {
                let n = self.u32()? as usize;
                self.bytes(n)?;
            }
            0x8 => {
                self.bytes(8)?;
            }
            0x4 => {
                self.bytes(4)?;
            }
            0x1 => {
                self.bytes(1)?;
            }
            _ => return Err(Error::Malformed(self.pos)),
        }
        Ok(())
    }
}
