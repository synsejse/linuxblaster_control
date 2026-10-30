//! Experimental bounded userspace transfers, firmware 1.7.250324.0910.
//! Transport completion, validation, processing and acoustic activation differ.
//! No automatic reset. SHA-256 integrity is not authenticity.
use crate::{
    X4,
    controls::Information,
    protocol::{Result, invalid, unsupported},
};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameWidth {
    Short,
    Extended,
}
impl FrameWidth {
    pub fn frame(self, payload: &[u8]) -> Result<Vec<u8>> {
        if payload.len() > 4096 || (self == Self::Short && payload.len() > 255)
        {
            return Err(invalid("Transfer frame bounds"));
        }
        let mut p = match self {
            Self::Short => vec![0x5a, 0x98, payload.len() as u8],
            Self::Extended => {
                let mut p = vec![0x5b, 0x98];
                p.extend((payload.len() as u16).to_le_bytes());
                p
            }
        };
        p.extend(payload);
        Ok(p)
    }
    fn overhead(self) -> u16 {
        match self {
            Self::Short => 6,
            Self::Extended => 8,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartReply {
    pub offset: u32,
    pub token: u32,
    pub slots: u8,
    pub resume: bool,
    pub packet_size: u16,
}
impl StartReply {
    pub fn decode(p: &[u8]) -> Result<Self> {
        if p.len() != 12 || p[0] != 2 || p[9] & 0x60 != 0 {
            return Err(invalid("Transfer start reply shape"));
        }
        Ok(Self {
            offset: word(p, 1),
            token: word(p, 5),
            slots: (p[9] & 31) + 1,
            resume: p[9] & 128 != 0,
            packet_size: u16::from_le_bytes([p[10], p[11]]),
        })
    }
}
fn word(p: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(p[at..at + 4].try_into().unwrap())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferInfo {
    pub total: u32,
    pub offset: u32,
    pub token: u32,
    pub slots: u8,
    pub resume: bool,
    pub packet_size: u16,
    pub crc: Option<u32>,
}
impl TransferInfo {
    pub fn decode(p: &[u8]) -> Result<Self> {
        if p.len() < 18
            || p[0] != 3
            || p[14] & 0x7f != 0
            || !matches!((p[17], p.len()), (0, 18) | (1, 22))
        {
            return Err(invalid("Transfer info shape/checksum kind"));
        }
        let info = Self {
            total: word(p, 1),
            offset: word(p, 5),
            token: word(p, 9),
            slots: p[13],
            resume: p[14] & 128 != 0,
            packet_size: u16::from_le_bytes([p[15], p[16]]),
            crc: if p[17] == 1 { Some(word(p, 18)) } else { None },
        };
        if info.slots == 0 || info.slots > 32 || info.offset > info.total {
            return Err(invalid("Transfer info bounds"));
        }
        Ok(info)
    }
    pub fn encode(self) -> Result<Vec<u8>> {
        if self.slots == 0 || self.slots > 32 || self.offset > self.total {
            return Err(invalid("Transfer info bounds"));
        }
        let mut p = vec![3];
        p.extend(self.total.to_le_bytes());
        p.extend(self.offset.to_le_bytes());
        p.extend(self.token.to_le_bytes());
        p.extend([self.slots, if self.resume { 128 } else { 0 }]);
        p.extend(self.packet_size.to_le_bytes());
        p.push(u8::from(self.crc.is_some()));
        if let Some(c) = self.crc {
            p.extend(c.to_le_bytes());
        }
        Ok(p)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowReply {
    pub status: u8,
    pub missing: u32,
    pub committed: u32,
}
impl WindowReply {
    pub fn decode(p: &[u8], slots: u8) -> Result<Self> {
        if p.len() != 10 || p[0] != 4 || slots == 0 || slots > 32 {
            return Err(invalid("Window reply shape"));
        }
        Ok(Self {
            status: p[1],
            missing: word(p, 2) & slot_mask(slots),
            committed: word(p, 6),
        })
    }
}
fn slot_mask(slots: u8) -> u32 {
    u32::MAX >> (32 - slots)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingStatus {
    Completed,
    Processing,
    Failed(u8),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessingResult {
    pub file_type: u8,
    pub token: u32,
    pub status: ProcessingStatus,
    pub result_byte: u8,
}
impl ProcessingResult {
    pub fn decode(p: &[u8], file_type: u8, token: u32) -> Result<Self> {
        if p.len() != 11
            || p[0] != 9
            || p[1] != file_type
            || word(p, 3) != token
            || p[8..] != [0, 0, 0]
        {
            return Err(invalid("Processing result identity/length"));
        }
        Ok(Self {
            file_type,
            token,
            status: match p[2] {
                0 => ProcessingStatus::Completed,
                1 => ProcessingStatus::Processing,
                x => ProcessingStatus::Failed(x),
            },
            result_byte: p[7],
        })
    }
}
/// Reflected IEEE CRC32; initial zero, supports incremental updates.
pub fn crc32(previous: u32, bytes: &[u8]) -> u32 {
    let mut c = !previous;
    for b in bytes {
        c ^= *b as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ if c & 1 != 0 { 0xedb88320 } else { 0 };
        }
    }
    !c
}
pub fn data_payload(
    width: FrameWidth,
    sequence: u8,
    data: &[u8],
) -> Result<Vec<u8>> {
    if sequence > 31
        || data.is_empty()
        || data.len() > u16::MAX as usize
        || (width == FrameWidth::Short && data.len() > 252)
    {
        return Err(invalid("Data slot/length"));
    }
    let mut p = vec![4, sequence];
    match width {
        FrameWidth::Short => p.push(data.len() as u8),
        FrameWidth::Extended => p.extend((data.len() as u16).to_le_bytes()),
    };
    p.extend(data);
    Ok(p)
}
#[derive(Debug, Clone, Copy)]
pub enum UploadKind {
    /// Stage bytes in /internal/upgrade/fwu.zif; does not apply firmware.
    FirmwareStage,
    Room11,
    Room12,
    Headphone,
    Metadata,
}
impl UploadKind {
    pub fn wire(self) -> u8 {
        match self {
            Self::FirmwareStage => 1,
            Self::Room11 => 0x11,
            Self::Room12 => 0x12,
            Self::Headphone => 0x14,
            Self::Metadata => 0x16,
        }
    }
}
/// Existing prepared vendor asset. No headmap generation or cloud credentials.
/// Contents are deliberately excluded from Debug.
pub struct PreparedUpload {
    kind: UploadKind,
    data: Vec<u8>,
}
impl PreparedUpload {
    pub fn new(kind: UploadKind, data: Vec<u8>) -> Result<Self> {
        if data.is_empty() || data.len() > u32::MAX as usize {
            return Err(invalid("Upload size"));
        }
        match kind {
            UploadKind::FirmwareStage => {
                if data.len() > 16 * 1024 * 1024 {
                    return Err(invalid("Staging host limit 16 MiB"));
                }
            }
            UploadKind::Metadata => {
                if data.len() > 512 {
                    return Err(invalid(
                        "Metadata exceeds firmware 512-byte limit",
                    ));
                }
            }
            UploadKind::Room11 | UploadKind::Room12 => {
                let (size, end) = match data.get(..4) {
                    Some(b"RURI") => (108, 116),
                    Some(b"RUR2") => (138, 146),
                    Some(b"RUR3") => (142, 150),
                    _ => return Err(invalid("Unknown room container")),
                };
                if data.len() < end + 8
                    || word(&data, 4) != size
                    || &data[end..end + 4] != b"RREN"
                    || word(&data, end + 4) as usize != data.len() - end - 8
                {
                    return Err(invalid("Room section lengths/trailer"));
                }
            }
            UploadKind::Headphone => {
                if data.len() < 12 || !data.windows(4).any(|x| x == b"HCP3") {
                    return Err(invalid("Prepared HCP3 container required"));
                }
            }
        }
        Ok(Self { kind, data })
    }
    pub fn kind(&self) -> UploadKind {
        self.kind
    }
}
/// Caller-retained identity and last confirmed committed offset for the same
/// prepared asset. A device offer is checked again before any resumed data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadCheckpoint {
    pub file_type: u8,
    pub total: u32,
    pub token: u32,
    pub offset: u32,
}
/// Explicit host timeouts, not firmware clock-unit conversions.
#[derive(Debug, Clone, Copy)]
pub struct ProcessingPolicy {
    pub timeout: Duration,
    pub poll_interval: Duration,
}
impl Default for ProcessingPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            poll_interval: Duration::from_secs(1),
        }
    }
}
impl ProcessingPolicy {
    fn validate(self) -> Result<()> {
        if self.timeout.is_zero()
            || self.timeout > Duration::from_secs(3600)
            || self.poll_interval.is_zero()
            || self.poll_interval > self.timeout
        {
            return Err(invalid("Processing timeout policy"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadOutcome {
    AlreadyPresent,
    /// Type01 reports staging completion, not flash application or readback.
    /// Firmware short writes can still advance its committed counter.
    Staged(ProcessingResult),
    Processed(ProcessingResult),
}
#[derive(Debug, Clone, Copy)]
pub enum DownloadKind {
    Log,
    RoomDescriptor,
    HeadphoneIdentifier,
    UserMetadata,
}
impl DownloadKind {
    fn wire(self) -> u8 {
        match self {
            Self::Log => 255,
            Self::RoomDescriptor => 0x93,
            Self::HeadphoneIdentifier => 0x95,
            Self::UserMetadata => 0x96,
        }
    }
}
/// Sanitizes all sixteen uninitialized stack bytes before returning data.
/// No raw room-descriptor persistence or Debug output is provided.
pub fn sanitize_download(
    kind: DownloadKind,
    mut bytes: Vec<u8>,
) -> Result<Information> {
    let expected = match kind {
        DownloadKind::RoomDescriptor => Some(154),
        DownloadKind::HeadphoneIdentifier => Some(116),
        DownloadKind::UserMetadata => None,
        DownloadKind::Log => None,
    };
    if expected.is_some_and(|n| bytes.len() != n) {
        return Err(invalid("Download record size"));
    }
    if matches!(kind, DownloadKind::UserMetadata) {
        UserMetadata::decode(&bytes)?;
    }
    if matches!(kind, DownloadKind::RoomDescriptor) {
        bytes[..8].fill(0);
        bytes[146..154].fill(0);
    }
    Ok(Information(bytes))
}
impl<T: Read + Write> X4<T> {
    pub fn transfer_types(&mut self) -> Result<Vec<u8>> {
        self.selector(0x98, &[8])
    }
    /// Firmware holds a single last-result cache, not a token-indexed history.
    pub fn transfer_processing(
        &mut self,
        file_type: u8,
        token: u32,
    ) -> Result<ProcessingResult> {
        let mut p = vec![9, file_type];
        p.extend(token.to_le_bytes());
        let r = self.selector(0x98, &p)?;
        ProcessingResult::decode(&r, file_type, token)
    }
    fn send_transfer(&mut self, width: FrameWidth, p: &[u8]) -> Result<()> {
        self.session.transfer_send(&width.frame(p)?)
    }
    fn transfer_ack(
        &mut self,
        selector: u8,
        offset: Option<u32>,
    ) -> Result<()> {
        let p = self
            .session
            .continuation(2, |f| f.payload.first() == Some(&0x98))?;
        if p.len() != 10
            || p[..3] != [0x98, 0, selector]
            || offset.is_some_and(|o| word(&p, 3) != o)
        {
            return Err(invalid("Transfer ACK context/status mismatch"));
        }
        Ok(())
    }
    /// Fresh prepared upload, bounded retries of explicitly missing slots only.
    /// Returns processing completion, never a claim that new acoustic filters activated.
    pub fn upload_prepared(
        &mut self,
        asset: &PreparedUpload,
    ) -> Result<UploadOutcome> {
        self.upload_prepared_with_policy(asset, ProcessingPolicy::default())
    }
    /// Host timing policy, independent of firmware's nominal one-second callbacks
    /// and 12/30-callback thresholds, which are subject to scheduling delay.
    pub fn upload_prepared_with_policy(
        &mut self,
        asset: &PreparedUpload,
        policy: ProcessingPolicy,
    ) -> Result<UploadOutcome> {
        policy.validate()?;
        self.session.begin_transfer()?;
        let result =
            self.upload_prepared_inner(asset, policy, None, &mut |_| true);
        self.session.end_transfer();
        if result.is_err() {
            self.session.usable = false;
        }
        result
    }
    /// Resume the same prepared bytes from a previously confirmed committed
    /// offset. The device must offer the same token and at least that offset.
    /// Callers must preserve the original asset; this API cannot infer its identity
    /// from an opaque device token. Fresh transfers never silently resume.
    pub fn resume_prepared(
        &mut self,
        asset: &PreparedUpload,
        checkpoint: UploadCheckpoint,
        policy: ProcessingPolicy,
    ) -> Result<UploadOutcome> {
        policy.validate()?;
        self.session.begin_transfer()?;
        let result = self.upload_prepared_inner(
            asset,
            policy,
            Some(checkpoint),
            &mut |_| true,
        );
        self.session.end_transfer();
        if result.is_err() {
            self.session.abort();
        }
        result
    }
    /// Receive resumable checkpoints after setup and each acknowledged shift.
    /// Return false to cancel locally and invalidate the session. No wire cancel
    /// is sent, and firmware retention is not guaranteed.
    pub fn upload_prepared_with_progress(
        &mut self,
        asset: &PreparedUpload,
        policy: ProcessingPolicy,
        mut progress: impl FnMut(UploadCheckpoint) -> bool,
    ) -> Result<UploadOutcome> {
        policy.validate()?;
        self.session.begin_transfer()?;
        let result =
            self.upload_prepared_inner(asset, policy, None, &mut progress);
        self.session.end_transfer();
        if result.is_err() {
            self.session.abort();
        }
        result
    }
    fn upload_prepared_inner(
        &mut self,
        asset: &PreparedUpload,
        policy: ProcessingPolicy,
        resume: Option<UploadCheckpoint>,
        progress: &mut impl FnMut(UploadCheckpoint) -> bool,
    ) -> Result<UploadOutcome> {
        let total = asset.data.len() as u32;
        let file_type = asset.kind.wire();
        let mut p = vec![2];
        p.extend(total.to_le_bytes());
        p.push(file_type);
        self.send_transfer(FrameWidth::Short, &p)?;
        let start = StartReply::decode(
            &self
                .session
                .continuation(0x98, |f| f.payload.first() == Some(&2))?,
        )?;
        let offset = match resume {
            Some(saved) => {
                if !start.resume
                    || saved.file_type != file_type
                    || saved.total != total
                    || saved.token != start.token
                    || saved.offset > start.offset
                    || saved.offset >= total
                    || start.offset > total
                    || (file_type == 1 && saved.offset != start.offset)
                {
                    return Err(invalid(
                        "Resume identity/committed limit mismatch",
                    ));
                }
                saved.offset
            }
            None if start.resume || start.offset != 0 => {
                return Err(unsupported(
                    "Device offers retained state; use resume_prepared with an explicit matching checkpoint",
                ));
            }
            None => 0,
        };
        let width = FrameWidth::Extended;
        let stride = start
            .packet_size
            .checked_sub(width.overhead())
            .filter(|n| *n > 0)
            .ok_or_else(|| invalid("Negotiated packet size"))?
            as usize;
        if start.packet_size > 4096
            || (width == FrameWidth::Short && stride > 252)
        {
            return Err(invalid(
                "Negotiated frame exceeds bounded implementation",
            ));
        }
        let info = TransferInfo {
            total,
            offset,
            token: start.token,
            slots: start.slots,
            resume: resume.is_some(),
            packet_size: start.packet_size,
            crc: Some(crc32(0, &asset.data)),
        };
        self.send_transfer(width, &info.encode()?)?;
        self.transfer_ack(3, None)?;
        let checkpoint = |offset| UploadCheckpoint {
            file_type,
            total,
            token: start.token,
            offset,
        };
        if !progress(checkpoint(offset)) {
            return Err(invalid("Upload cancelled locally"));
        }
        let mut committed = offset as usize;
        while committed < asset.data.len() {
            let end = (committed + stride * start.slots as usize)
                .min(asset.data.len());
            let chunks: Vec<_> =
                asset.data[committed..end].chunks(stride).collect();
            for (i, c) in chunks.iter().enumerate() {
                self.send_transfer(width, &data_payload(width, i as u8, c)?)?;
            }
            if end == asset.data.len() {
                self.send_transfer(width, &[6, 128, (chunks.len() - 1) as u8])?;
            }
            let mut retries = 0;
            loop {
                let p = self
                    .session
                    .continuation(0x98, |f| f.payload.first() == Some(&4))?;
                let reply = WindowReply::decode(&p, start.slots)?;
                match reply.status {
                    0x81 => {
                        if reply.committed != committed as u32
                            || reply.missing & slot_mask(chunks.len() as u8)
                                == 0
                        {
                            return Err(invalid(
                                "Recovery offset/bitmap mismatch",
                            ));
                        }
                        if retries == 32 {
                            return Err(unsupported(
                                "Missing-slot retry budget exhausted; reconnect before explicit resume",
                            ));
                        }
                        retries += 1;
                        for (i, c) in chunks.iter().enumerate() {
                            if reply.missing & (1 << i) != 0 {
                                self.send_transfer(
                                    width,
                                    &data_payload(width, i as u8, c)?,
                                )?;
                            }
                        }
                        if end == asset.data.len() {
                            self.send_transfer(
                                width,
                                &[6, 128, (chunks.len() - 1) as u8],
                            )?;
                        }
                    }
                    0x10 => return Ok(UploadOutcome::AlreadyPresent),
                    0 => {
                        if reply.committed != end as u32
                            || reply.missing & slot_mask(chunks.len() as u8)
                                != 0
                        {
                            return Err(invalid(
                                "Window completion offset/bitmap mismatch",
                            ));
                        }
                        break;
                    }
                    _ => {
                        return Err(invalid(format!(
                            "Transfer validator/window status {:02x}",
                            reply.status
                        )));
                    }
                }
            }
            let mut shift = vec![5];
            shift.extend((end as u32).to_le_bytes());
            self.send_transfer(width, &shift)?;
            self.transfer_ack(5, Some(end as u32))?;
            committed = end;
            if !progress(checkpoint(end as u32)) {
                return Err(invalid("Upload cancelled locally"));
            }
        }
        self.wait_processing(file_type, start.token, policy)
            .map(|result| {
                if file_type == 1 {
                    UploadOutcome::Staged(result)
                } else {
                    UploadOutcome::Processed(result)
                }
            })
    }
    fn wait_processing(
        &mut self,
        file_type: u8,
        token: u32,
        policy: ProcessingPolicy,
    ) -> Result<ProcessingResult> {
        let deadline = Instant::now() + policy.timeout;
        loop {
            let until = deadline.min(Instant::now() + policy.poll_interval);
            let reply =
                self.session.continuation_frame_until(0x98, until, |f| {
                    let p = &f.payload;
                    p.len() >= 7
                        && p[0] == 9
                        && p[1] == file_type
                        && word(p, 3) == token
                });
            match reply {
                Ok(frame) => {
                    let result = ProcessingResult::decode(
                        &frame.payload,
                        file_type,
                        token,
                    )?;
                    match result.status {
                        ProcessingStatus::Completed => return Ok(result),
                        ProcessingStatus::Failed(s) => {
                            return Err(invalid(format!(
                                "Processing failed: {s:02x}"
                            )));
                        }
                        ProcessingStatus::Processing => {}
                    }
                }
                Err(e)
                    if e.downcast_ref::<std::io::Error>().is_some_and(
                        |e| e.kind() == std::io::ErrorKind::TimedOut,
                    ) =>
                {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    let mut query = vec![9, file_type];
                    query.extend(token.to_le_bytes());
                    // Type01 staging sends an asynchronous success but does not
                    // populate the firmware's query cache on this path.
                    if file_type != 1 {
                        self.send_transfer(FrameWidth::Short, &query)?;
                    }
                }
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Err(invalid("Processing deadline"));
            }
        }
    }
    pub fn download_coefficients(&mut self) -> Result<Information> {
        Err(unsupported(
            "91/94 unsupported through this firmware download path; 93/95 export descriptors only",
        ))
    }
}
impl<T: Read + Write> X4<T> {
    /// Fresh multiwindow download with missing-slot recovery, at most 16 MiB.
    /// Host policy: 60 seconds overall, one second per recovery interval, ten retries.
    /// No partial data escapes. Both data frame markers are decoded.
    /// Room descriptor stale-stack regions are sanitized after CRC verification.
    pub fn download_asset(
        &mut self,
        kind: DownloadKind,
    ) -> Result<Information> {
        self.session.begin_transfer()?;
        let result = self.download_asset_inner(kind);
        self.session.end_transfer();
        if result.is_err() {
            self.session.usable = false;
        }
        result
    }
    fn download_asset_inner(
        &mut self,
        kind: DownloadKind,
    ) -> Result<Information> {
        let w = FrameWidth::Extended;
        let file_type = kind.wire();
        self.send_transfer(w, &[2, 0, 0, 0, 0, file_type])?;
        let ack = self
            .session
            .continuation(2, |f| f.payload.first() == Some(&0x98))?;
        if ack.len() != 10
            || ack[..3] != [0x98, 0, 2]
            || ack[7] != (file_type & 127)
        {
            return Err(invalid("Download start rejected/identity mismatch"));
        }
        let packet = self.session.max_payload.min(4096) as u16;
        if packet <= 8 {
            return Err(invalid("Download packet limit"));
        }
        let mut start = vec![2, 0, 0, 0, 0, 0, 0, 0, 0, 31];
        start.extend(packet.to_le_bytes());
        self.send_transfer(w, &start)?;
        let p = self
            .session
            .continuation(0x98, |f| f.payload.first() == Some(&3))?;
        let info = TransferInfo::decode(&p)?;
        if info.offset != 0 || info.resume {
            return Err(unsupported("Resumed download not implemented"));
        }
        if info.packet_size <= 8
            || info.packet_size > packet
            || info.total == 0
            || info.total > 16 * 1024 * 1024
        {
            return Err(unsupported(
                "Download exceeds 16 MiB or negotiated packet bounds",
            ));
        }
        // ACK03 uses flags=1, ACK05 uses zero context, matching retained export capture.
        self.session.transfer_send(&[
            0x5b, 2, 10, 0, 0x98, 0, 3, 1, 0, 0, 0, 0, 0, 0,
        ])?;
        let mut data = Vec::with_capacity(info.total as usize);
        let deadline = Instant::now() + Duration::from_secs(60);
        while data.len() < info.total as usize {
            let mut slots: Vec<Option<Vec<u8>>> =
                vec![None; info.slots as usize];
            let mut last = None;
            let mut width = None;
            let mut retries = 0;
            loop {
                let until =
                    deadline.min(Instant::now() + Duration::from_secs(1));
                let received =
                    self.session.continuation_frame_until(0x98, until, |f| {
                        matches!(f.payload.first(), Some(4 | 6))
                    });
                let timed_out = match received {
                    Ok(f) => {
                        let p = &f.payload;
                        if p[0] == 4 {
                            let w = match f.marker {
                                0x5a => FrameWidth::Short,
                                0x5b => FrameWidth::Extended,
                                _ => {
                                    return Err(invalid("Unknown data marker"));
                                }
                            };
                            if width.is_some_and(|old| old != w) {
                                return Err(invalid(
                                    "Mixed data widths in window",
                                ));
                            }
                            width = Some(w);
                            let header =
                                if w == FrameWidth::Short { 3 } else { 4 };
                            if p.len() < header {
                                return Err(invalid("Download data header"));
                            }
                            let slot = (p[1] & 31) as usize;
                            let count = if header == 3 {
                                p[2] as usize
                            } else {
                                u16::from_le_bytes([p[2], p[3]]) as usize
                            };
                            let stride = info.packet_size as usize
                                - w.overhead() as usize;
                            let remaining = info.total as usize - data.len();
                            let expected = remaining
                                .saturating_sub(slot * stride)
                                .min(stride);
                            if slot >= slots.len()
                                || count == 0
                                || count != expected
                                || p.len() != header + count
                            {
                                return Err(invalid(
                                    "Download slot length/total bounds",
                                ));
                            }
                            if let Some(old) = &slots[slot] {
                                if old != &p[header..] {
                                    return Err(invalid(
                                        "Conflicting duplicate download slot",
                                    ));
                                }
                            } else {
                                slots[slot] = Some(p[header..].to_vec());
                            }
                        } else {
                            if p.len() != 3
                                || p[1] != 128
                                || p[2] > 31
                                || p[2] >= info.slots
                            {
                                return Err(invalid("Download end marker"));
                            }
                            if last.is_some_and(|old| old != p[2] as usize) {
                                return Err(invalid("Conflicting end marker"));
                            }
                            last = Some(p[2] as usize);
                        }
                        false
                    }
                    Err(e)
                        if e.downcast_ref::<std::io::Error>().is_some_and(
                            |e| e.kind() == std::io::ErrorKind::TimedOut,
                        ) =>
                    {
                        true
                    }
                    Err(e) => return Err(e),
                };
                if Instant::now() >= deadline {
                    return Err(invalid("Download deadline"));
                }
                let stride = info.packet_size as usize
                    - width.unwrap_or(w).overhead() as usize;
                let remaining = info.total as usize - data.len();
                let n = remaining.div_ceil(stride).min(slots.len());
                let final_window = remaining <= stride * slots.len();
                if last.is_some_and(|slot| !final_window || slot + 1 != n) {
                    return Err(invalid("End marker disagrees with total"));
                }
                let missing =
                    slots[..n].iter().enumerate().fold(0u32, |m, (i, s)| {
                        m | if s.is_none() { 1 << i } else { 0 }
                    });
                let complete =
                    missing == 0 && (!final_window || last.is_some());
                if complete || timed_out || last.is_some() {
                    if !complete {
                        if retries == 10 {
                            return Err(invalid(
                                "Download recovery budget exhausted",
                            ));
                        }
                        retries += 1;
                    }
                    let committed = if complete {
                        data.len()
                            + slots[..n]
                                .iter()
                                .flatten()
                                .map(Vec::len)
                                .sum::<usize>()
                    } else {
                        data.len()
                    };
                    let mut reply = vec![4, if complete { 0 } else { 0x81 }];
                    reply.extend((missing | !slot_mask(n as u8)).to_le_bytes());
                    reply.extend((committed as u32).to_le_bytes());
                    self.send_transfer(w, &reply)?;
                    if !complete {
                        // A repeated marker triggers another response; ordinary recovered
                        // data is collected until all required slots have arrived.
                        last = None;
                        continue;
                    }
                    let shift = self.session.continuation(0x98, |f| {
                        f.payload.first() == Some(&5)
                    })?;
                    if shift.len() != 5 || word(&shift, 1) != committed as u32 {
                        return Err(invalid("Download committed offset"));
                    }
                    self.session.transfer_send(&[
                        0x5b, 2, 10, 0, 0x98, 0, 5, 0, 0, 0, 0, 0, 0, 0,
                    ])?;
                    for slot in slots.into_iter().take(n) {
                        data.extend(slot.expect("complete window"));
                    }
                    break;
                }
            }
        }
        if info.crc.is_some_and(|c| crc32(0, &data) != c) {
            return Err(invalid("Download IEEE CRC32 mismatch"));
        }
        sanitize_download(kind, data)
    }
}
/// User-profile metadata, not acoustic coefficients. Private fields are never Debug logged.
pub struct UserMetadata {
    pub user_id: Vec<u8>,
    pub description: Vec<u8>,
    pub timestamp: u64,
    pub parent_profile_id: Vec<u8>,
    pub generation: Vec<u8>,
}
impl UserMetadata {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut p = vec![0; 4];
        fn string(p: &mut Vec<u8>, s: &[u8]) -> Result<()> {
            if s.contains(&0) {
                return Err(invalid("Metadata string contains NUL"));
            }
            p.extend(s);
            p.push(0);
            Ok(())
        }
        string(&mut p, &self.user_id)?;
        string(&mut p, &self.description)?;
        p.extend(self.timestamp.to_le_bytes());
        string(&mut p, &self.parent_profile_id)?;
        string(&mut p, &self.generation)?;
        if p.len() > 512 {
            return Err(invalid("Metadata exceeds 512 bytes"));
        }
        let size = p.len() as u32;
        p[..4].copy_from_slice(&size.to_le_bytes());
        Ok(p)
    }
    pub fn decode(p: &[u8]) -> Result<Self> {
        if p.len() < 16 || p.len() > 512 || word(p, 0) as usize != p.len() {
            return Err(invalid("Metadata total length"));
        }
        fn string(p: &[u8], pos: &mut usize) -> Result<Vec<u8>> {
            let n = p[*pos..]
                .iter()
                .position(|x| *x == 0)
                .ok_or_else(|| invalid("Unterminated metadata string"))?;
            let s = p[*pos..*pos + n].to_vec();
            *pos += n + 1;
            Ok(s)
        }
        let mut pos = 4;
        let user_id = string(p, &mut pos)?;
        let description = string(p, &mut pos)?;
        if p.len() - pos < 8 {
            return Err(invalid("Truncated metadata timestamp"));
        }
        let timestamp = u64::from_le_bytes(p[pos..pos + 8].try_into()?);
        pos += 8;
        let parent_profile_id = string(p, &mut pos)?;
        let generation = string(p, &mut pos)?;
        if pos != p.len() {
            return Err(invalid("Metadata trailing bytes"));
        }
        Ok(Self {
            user_id,
            description,
            timestamp,
            parent_profile_id,
            generation,
        })
    }
}
impl std::fmt::Debug for UserMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UserMetadata([redacted])")
    }
}
impl PreparedUpload {
    pub fn metadata(value: &UserMetadata) -> Result<Self> {
        Self::new(UploadKind::Metadata, value.encode()?)
    }
}
/// Parsed room export contains only initialized fields. Numeric mode bytes have no inferred acoustic label.
#[derive(Debug)]
pub struct RoomDescriptor {
    pub identifier: Information,
    pub mode: [u8; 10],
    pub field_32: Information,
    pub field_72: Information,
}
impl RoomDescriptor {
    pub fn decode(p: &[u8]) -> Result<Self> {
        if p.len() != 154 {
            return Err(invalid("Room descriptor length"));
        }
        Ok(Self {
            identifier: Information(p[8..40].to_vec()),
            mode: p[40..50].try_into()?,
            field_32: Information(p[50..114].to_vec()),
            field_72: Information(p[114..146].to_vec()),
        })
    }
}
/// Known start codec, including explicitly experimental file-backed upload types.
#[derive(Debug, Clone, Copy)]
pub enum TransferStart {
    Upload { kind: UploadKind, total: u32 },
    Download(DownloadKind),
    ExperimentalFileUpload { file_type: u8, total: u32 },
}
impl TransferStart {
    pub fn encode(self) -> Result<[u8; 6]> {
        let (total, kind) = match self {
            Self::Upload { kind, total } => (total, kind.wire()),
            Self::Download(kind) => (0, kind.wire()),
            Self::ExperimentalFileUpload { file_type, total } => {
                if file_type != 1 && !(0x20..=0x3f).contains(&file_type) {
                    return Err(invalid("Unknown experimental upload type"));
                }
                (total, file_type)
            }
        };
        if kind & 128 == 0 && total == 0 {
            return Err(invalid("Zero-size upload"));
        }
        let mut p = [2, 0, 0, 0, 0, kind];
        p[1..5].copy_from_slice(&total.to_le_bytes());
        Ok(p)
    }
}
