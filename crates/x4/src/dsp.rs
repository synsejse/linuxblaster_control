//! DSP tuples preserve float32 bits. Multi-write and profile writes are not atomic.
//! Command `14` (alternate DSP write) is rejected by X4 firmware and not exposed.
use crate::{
    Output, X4,
    protocol::{Result, invalid},
};
use std::io::{Read, Write};
/// Command17 flags bits1..3: speaker=0, headphone=1, current=7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileBank {
    Speaker,
    Headphone,
    Current,
}
impl ProfileBank {
    pub(crate) fn flags(self) -> u8 {
        match self {
            Self::Speaker => 0,
            Self::Headphone => 2,
            Self::Current => 14,
        }
    }
}
impl From<Output> for ProfileBank {
    fn from(output: Output) -> Self {
        match output {
            Output::Speaker => Self::Speaker,
            Output::Headphone => Self::Headphone,
        }
    }
}
/// Command17 flags bit0: writable cache=0, immutable factory data=1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    Writable,
    Factory,
}
impl ProfileSource {
    pub(crate) fn flag(self) -> u8 {
        u8::from(self == Self::Factory)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Parameter {
    pub module: u8,
    pub parameter: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DspValue {
    pub parameter: Parameter,
    pub bits: u32,
}
impl DspValue {
    pub fn value(self) -> f32 {
        f32::from_bits(self.bits)
    }
}
pub fn decode_fragment(p: &[u8]) -> Result<(bool, Vec<DspValue>)> {
    if p.len() < 2
        || p[0] > 9
        || p[1] > 1
        || p.len() != 2 + 6 * p[0] as usize
        || (p[0] == 0 && p[1] != 0)
    {
        return Err(invalid("DSP fragment shape"));
    }
    Ok((
        p[1] != 0,
        p[2..]
            .as_chunks::<6>()
            .0
            .iter()
            .map(|x| DspValue {
                parameter: Parameter {
                    module: x[0],
                    parameter: x[1],
                },
                bits: u32::from_le_bytes(x[2..6].try_into().unwrap()),
            })
            .collect(),
    ))
}
impl<T: Read + Write> X4<T> {
    /// `11 count,(module,param)*` reads current values; `alternate=true` uses
    /// `13` for defaults. Replies are `count,more,(module,param,value32le)*`.
    pub fn dsp_read(
        &mut self,
        parameters: &[Parameter],
        alternate: bool,
    ) -> Result<Vec<DspValue>> {
        if parameters.is_empty() || parameters.len() > 28 {
            return Err(invalid("DSP query count 1..28"));
        }
        let mut expected = std::collections::HashSet::new();
        for p in parameters {
            if !expected.insert(*p) {
                return Err(invalid("Duplicate DSP request"));
            }
        }
        let c = if alternate { 0x13 } else { 0x11 };
        let mut q = vec![parameters.len() as u8];
        for p in parameters {
            q.extend([p.module, p.parameter]);
        }
        let mut p = self.session.get(c, &q, |_| true)?;
        let mut out = vec![];
        loop {
            let (more, values) = decode_fragment(&p)?;
            for v in values {
                if !expected.remove(&v.parameter) {
                    return Err(invalid("Unexpected/duplicate DSP tuple"));
                }
                out.push(v);
            }
            if !more {
                break;
            }
            if expected.is_empty() {
                return Err(invalid("Spurious DSP continuation"));
            }
            p = self.session.continuation(c, |_| true)?;
        }
        if !expected.is_empty() {
            return Err(invalid("Missing DSP tuples"));
        }
        Ok(out)
    }
    /// `12 count,(module,param,value32le)*`: ACK confirms cache/queue acceptance,
    /// not DSP consumption. Failure can leave partially changed cached values.
    pub fn dsp_write(&mut self, values: &[DspValue]) -> Result<()> {
        if values.is_empty() {
            return Err(invalid("Empty DSP write"));
        }
        for chunk in values.chunks(9) {
            let mut p = vec![chunk.len() as u8];
            for v in chunk {
                p.extend([v.parameter.module, v.parameter.parameter]);
                p.extend(v.bits.to_le_bytes());
            }
            self.session.set(0x12, &p)?;
        }
        Ok(())
    }
    pub fn dsp_range(&mut self) -> Result<[f32; 3]> {
        let p = self.fixed_selector(0x15, &[0], 16)?;
        if p[..4] != [0, 1, 0x96, 0x0b] {
            return Err(invalid("DSP range identity"));
        }
        Ok(std::array::from_fn(|i| {
            f32::from_le_bytes(p[4 + i * 4..8 + i * 4].try_into().unwrap())
        }))
    }
    /// Firmware stored profile bytes, independent of app preset names.
    pub fn write_profile_data(
        &mut self,
        output: Output,
        index: u8,
        data: &[u8],
    ) -> Result<()> {
        check_profile_index(index)?;
        let count = data
            .first_chunk::<4>()
            .map(|c| u32::from_le_bytes(*c) as usize);
        if !count.is_some_and(|n| n <= 12 && data.len() == 4 + n * 6) {
            return Err(invalid("Profile bounds"));
        }
        let stride = self
            .session
            .max_payload
            .checked_sub(7)
            .filter(|x| *x > 0)
            .ok_or_else(|| invalid("Profile packet limit"))?;
        let count = data.len().div_ceil(stride);
        if count > 128 {
            return Err(invalid("Too many profile blocks"));
        }
        for (i, chunk) in data.chunks(stride).enumerate() {
            let mut p = vec![0, 2, index, ProfileBank::from(output).flags()];
            p.extend((data.len() as u16).to_le_bytes());
            p.push(i as u8 | if i + 1 < count { 128 } else { 0 });
            p.extend(chunk);
            self.session.set(0x17, &p)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy)]
pub enum ProfileField {
    Name,
    Auxiliary,
}
impl ProfileField {
    /// Maximum characters (two wire bytes each).
    fn limit(self) -> usize {
        match self {
            Self::Name => 32,
            Self::Auxiliary => 8,
        }
    }
    /// Command `18` selector for (write, read, info).
    fn selectors(self) -> (u8, u8, u8) {
        match self {
            Self::Name => (0, 1, 2),
            Self::Auxiliary => (3, 4, 5),
        }
    }
}
fn check_profile_index(index: u8) -> Result<()> {
    if index > 3 {
        return Err(invalid("Profile index outside 0..3"));
    }
    Ok(())
}
impl<T: Read + Write> X4<T> {
    pub fn profile_field_info(
        &mut self,
        index: u8,
        field: ProfileField,
    ) -> Result<Vec<u8>> {
        check_profile_index(index)?;
        self.fixed_selector(0x18, &[field.selectors().2, 2, index], 7)
    }
    /// Two-byte wire characters; firmware stores only low bytes in both banks.
    pub fn profile_field(
        &mut self,
        index: u8,
        field: ProfileField,
    ) -> Result<Vec<u8>> {
        check_profile_index(index)?;
        let prefix = [field.selectors().1, 2, index];
        let mut p = self.selector(0x18, &prefix)?;
        let mut out = vec![];
        let mut total = None;
        for _ in 0..32 {
            if p.len() < 6 || p[..3] != prefix || p[5] as usize * 2 != out.len()
            {
                return Err(invalid("Profile field offset/identity"));
            }
            let size = (p[3] & 127) as usize * 2;
            if size > field.limit() * 2
                || p.len() != 6 + p[4] as usize * 2
                || *total.get_or_insert(size) != size
            {
                return Err(invalid("Profile field length"));
            }
            out.extend(&p[6..]);
            if out.len() > size {
                return Err(invalid("Profile field overflow"));
            }
            if p[3] & 128 == 0 {
                if out.len() != size {
                    return Err(invalid("Incomplete profile field"));
                }
                return Ok(out);
            }
            p = self
                .session
                .continuation(0x18, |f| f.payload.starts_with(&prefix))?;
        }
        Err(invalid("Too many field blocks"))
    }
    /// Header: selector,02,index,total|more,count,offset, then two-byte characters.
    /// Legacy argument names: block is total|more, total is count, count is offset.
    /// Nonfinal ACK80 retains bytes and is expected, not rollback. Both banks change.
    pub fn write_profile_field_block(
        &mut self,
        index: u8,
        field: ProfileField,
        block: u8,
        total: u8,
        count: u8,
        data: &[u8],
    ) -> Result<()> {
        check_profile_index(index)?;
        if (block & 127) as usize > field.limit()
            || count as usize + total as usize > (block & 127) as usize
            || data.len() != total as usize * 2
            || data.as_chunks::<2>().0.iter().any(|c| c[1] != 0)
        {
            return Err(invalid("Profile field bounds or non-byte character"));
        }
        let mut p = vec![field.selectors().0, 2, index, block, total, count];
        p.extend(data);
        self.session.prepare(0x18, &p)?;
        let reply = self
            .session
            .wait(|f| f.command == 2 && f.payload.first() == Some(&0x18))?;
        let expected = if block & 128 != 0 { 0x80 } else { 0 };
        if reply.payload.len() != 10 || reply.payload[1] != expected {
            self.session.abort();
            return Err(invalid("Profile field acknowledgement"));
        }
        Ok(())
    }
    /// Write a byte-string field using the firmware's shared sequential assembly.
    pub fn write_profile_field(
        &mut self,
        index: u8,
        field: ProfileField,
        value: &[u8],
    ) -> Result<()> {
        check_profile_index(index)?;
        if value.len() > field.limit() {
            return Err(invalid("Profile field bounds"));
        }
        let stride = self.session.max_payload.saturating_sub(6) / 2;
        if stride == 0 {
            return Err(invalid("Profile packet limit"));
        }
        if value.is_empty() {
            return self.write_profile_field_block(index, field, 0, 0, 0, &[]);
        }
        for (i, chunk) in value.chunks(stride).enumerate() {
            let offset = i * stride;
            let more = offset + chunk.len() < value.len();
            let pairs: Vec<_> = chunk.iter().flat_map(|b| [*b, 0]).collect();
            self.write_profile_field_block(
                index,
                field,
                value.len() as u8 | if more { 128 } else { 0 },
                chunk.len() as u8,
                offset as u8,
                &pairs,
            )?;
        }
        Ok(())
    }
    /// Firmware flags/index fields, not local app profile metadata.
    pub fn set_profile_information(&mut self, fields: [u8; 4]) -> Result<()> {
        if fields[0] & 1 != 0 || fields[1] > 3 {
            return Err(invalid("Profile flags/index"));
        }
        let mut p = vec![0, 2];
        p.extend(fields);
        self.session.set(0x1d, &p)
    }
}
