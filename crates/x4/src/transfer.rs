//! Experimental bounded userspace transfers, firmware 1.7.250324.0910.
//! Transport completion, validation, processing and acoustic activation differ.
//! No automatic reset. SHA-256 integrity is not authenticity.
use crate::{
    X4,
    controls::Information,
    error::{Result, invalid, unsupported},
    protocol::{Frame, word},
};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};
/// Upload frames use the extended header whatever their size.
const UPLOAD_WIDTH: FrameWidth = FrameWidth::Extended;
/// Largest asset the host stages or downloads.
const ASSET_LIMIT: usize = 16 * 1024 * 1024;
/// Resends of an upload window's missing slots before giving up.
const UPLOAD_RETRIES: u8 = 32;
/// Recovery requests in a download window before giving up.
const DOWNLOAD_RETRIES: u8 = 10;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a download waits for a frame before asking again.
const DOWNLOAD_POLL: Duration = Duration::from_secs(1);

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
    fn from_marker(marker: u8) -> Result<Self> {
        match marker {
            0x5a => Ok(Self::Short),
            0x5b => Ok(Self::Extended),
            _ => Err(invalid("Unknown data marker")),
        }
    }
    fn overhead(self) -> u16 {
        match self {
            Self::Short => 6,
            Self::Extended => 8,
        }
    }
    /// Bytes before a data slot's contents: `04 slot length`.
    fn data_header(self) -> usize {
        match self {
            Self::Short => 3,
            Self::Extended => 4,
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
        Self {
            total: word(p, 1),
            offset: word(p, 5),
            token: word(p, 9),
            slots: p[13],
            resume: p[14] & 128 != 0,
            packet_size: u16::from_le_bytes([p[15], p[16]]),
            crc: (p[17] == 1).then(|| word(p, 18)),
        }
        .checked()
    }
    pub fn encode(self) -> Result<Vec<u8>> {
        self.checked()?;
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
    fn checked(self) -> Result<Self> {
        if self.slots == 0 || self.slots > 32 || self.offset > self.total {
            return Err(invalid("Transfer info bounds"));
        }
        Ok(self)
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
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = (c >> 1) ^ if c & 1 != 0 { 0xedb8_8320 } else { 0 };
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
    }
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
                if data.len() > ASSET_LIMIT {
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
    match kind {
        DownloadKind::RoomDescriptor if bytes.len() == 154 => {
            bytes[..8].fill(0);
            bytes[146..154].fill(0);
        }
        DownloadKind::HeadphoneIdentifier if bytes.len() == 116 => {}
        DownloadKind::UserMetadata => {
            UserMetadata::decode(&bytes)?;
        }
        DownloadKind::Log => {}
        _ => return Err(invalid("Download record size")),
    }
    Ok(Information(bytes))
}
/// The `09 type token` query for a processing result.
fn processing_query(file_type: u8, token: u32) -> Vec<u8> {
    let mut p = vec![9, file_type];
    p.extend(token.to_le_bytes());
    p
}
/// Where an upload starts: zero, or a checkpoint that matches the device's offer.
fn resume_offset(
    start: StartReply,
    resume: Option<UploadCheckpoint>,
    file_type: u8,
    total: u32,
) -> Result<u32> {
    match resume {
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
            Ok(saved.offset)
        }
        None if start.resume || start.offset != 0 => Err(unsupported(
            "Device offers retained state; use resume_prepared with an explicit matching checkpoint",
        )),
        None => Ok(0),
    }
}
/// Data bytes per upload slot at the negotiated packet size.
fn upload_stride(packet_size: u16) -> Result<usize> {
    let stride = packet_size
        .checked_sub(UPLOAD_WIDTH.overhead())
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid("Negotiated packet size"))?;
    if packet_size > 4096 {
        return Err(invalid("Negotiated frame exceeds bounded implementation"));
    }
    Ok(stride as usize)
}
/// The slots of one upload window and the range of the asset they cover.
struct UploadWindow<'a> {
    chunks: Vec<&'a [u8]>,
    committed: u32,
    end: u32,
    last: bool,
}
impl UploadWindow<'_> {
    fn mask(&self) -> u32 {
        slot_mask(self.chunks.len() as u8)
    }
}
/// A host ACK for a download step, in the extended frame the device expects.
fn download_ack(selector: u8, flags: u8) -> [u8; 14] {
    [0x5b, 2, 10, 0, 0x98, 0, selector, flags, 0, 0, 0, 0, 0, 0]
}
/// The slots of one download window, which can arrive in any order.
struct DownloadWindow {
    slots: Vec<Option<Vec<u8>>>,
    /// The slot named by the end marker.
    last: Option<usize>,
    /// The frame width of this window's data, once one has arrived.
    width: Option<FrameWidth>,
    packet_size: usize,
    /// Bytes of the asset still to come, this window included.
    remaining: usize,
}
impl DownloadWindow {
    fn new(info: &TransferInfo, received: usize) -> Self {
        Self {
            slots: vec![None; info.slots as usize],
            last: None,
            width: None,
            packet_size: info.packet_size as usize,
            remaining: info.total as usize - received,
        }
    }
    fn stride(&self, width: FrameWidth) -> usize {
        self.packet_size - width.overhead() as usize
    }
    fn accept(&mut self, frame: &Frame) -> Result<()> {
        if frame.payload[0] == 4 {
            self.accept_data(frame)
        } else {
            self.accept_end(&frame.payload)
        }
    }
    fn accept_data(&mut self, frame: &Frame) -> Result<()> {
        let width = FrameWidth::from_marker(frame.marker)?;
        if self.width.is_some_and(|old| old != width) {
            return Err(invalid("Mixed data widths in window"));
        }
        self.width = Some(width);
        let p = &frame.payload;
        let header = width.data_header();
        if p.len() < header {
            return Err(invalid("Download data header"));
        }
        let slot = (p[1] & 31) as usize;
        let count = match width {
            FrameWidth::Short => p[2] as usize,
            FrameWidth::Extended => u16::from_le_bytes([p[2], p[3]]) as usize,
        };
        let stride = self.stride(width);
        let expected = self.remaining.saturating_sub(slot * stride).min(stride);
        if slot >= self.slots.len()
            || count == 0
            || count != expected
            || p.len() != header + count
        {
            return Err(invalid("Download slot length/total bounds"));
        }
        match &self.slots[slot] {
            Some(old) if old != &p[header..] => {
                Err(invalid("Conflicting duplicate download slot"))
            }
            Some(_) => Ok(()),
            None => {
                self.slots[slot] = Some(p[header..].to_vec());
                Ok(())
            }
        }
    }
    fn accept_end(&mut self, p: &[u8]) -> Result<()> {
        if p.len() != 3 || p[1] != 128 || p[2] as usize >= self.slots.len() {
            return Err(invalid("Download end marker"));
        }
        let slot = p[2] as usize;
        if self.last.is_some_and(|old| old != slot) {
            return Err(invalid("Conflicting end marker"));
        }
        self.last = Some(slot);
        Ok(())
    }
    /// How many slots this window needs, and whether it is the asset's last.
    fn extent(&self) -> (usize, bool) {
        let stride = self.stride(self.width.unwrap_or(FrameWidth::Extended));
        (
            self.remaining.div_ceil(stride).min(self.slots.len()),
            self.remaining <= stride * self.slots.len(),
        )
    }
    /// Bitmap of the first `needed` slots that have not arrived.
    fn missing(&self, needed: usize) -> u32 {
        self.slots[..needed]
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.is_none())
            .fold(0, |mask, (i, _)| mask | 1 << i)
    }
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
        let r = self.selector(0x98, &processing_query(file_type, token))?;
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
    /// Run one transfer exchange; a failure invalidates the session.
    fn transfer<R>(
        &mut self,
        exchange: impl FnOnce(&mut Self) -> Result<R>,
    ) -> Result<R> {
        self.session.begin_transfer()?;
        let result = exchange(self);
        self.session.end_transfer();
        if result.is_err() {
            self.session.abort();
        }
        result
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
        self.upload(asset, policy, None, |_| true)
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
        self.upload(asset, policy, Some(checkpoint), |_| true)
    }
    /// Receive resumable checkpoints after setup and each acknowledged shift.
    /// Return false to cancel locally and invalidate the session. No wire cancel
    /// is sent, and firmware retention is not guaranteed.
    pub fn upload_prepared_with_progress(
        &mut self,
        asset: &PreparedUpload,
        policy: ProcessingPolicy,
        progress: impl FnMut(UploadCheckpoint) -> bool,
    ) -> Result<UploadOutcome> {
        self.upload(asset, policy, None, progress)
    }
    fn upload(
        &mut self,
        asset: &PreparedUpload,
        policy: ProcessingPolicy,
        resume: Option<UploadCheckpoint>,
        mut progress: impl FnMut(UploadCheckpoint) -> bool,
    ) -> Result<UploadOutcome> {
        policy.validate()?;
        self.transfer(|x4| {
            x4.upload_asset(asset, policy, resume, &mut progress)
        })
    }
    fn upload_asset(
        &mut self,
        asset: &PreparedUpload,
        policy: ProcessingPolicy,
        resume: Option<UploadCheckpoint>,
        progress: &mut impl FnMut(UploadCheckpoint) -> bool,
    ) -> Result<UploadOutcome> {
        let total = asset.data.len() as u32;
        let file_type = asset.kind.wire();
        let request = TransferStart::Upload {
            kind: asset.kind,
            total,
        }
        .encode()?;
        self.send_transfer(FrameWidth::Short, &request)?;
        let start = StartReply::decode(
            &self
                .session
                .continuation(0x98, |f| f.payload.first() == Some(&2))?,
        )?;
        let offset = resume_offset(start, resume, file_type, total)?;
        let stride = upload_stride(start.packet_size)?;
        let info = TransferInfo {
            total,
            offset,
            token: start.token,
            slots: start.slots,
            resume: resume.is_some(),
            packet_size: start.packet_size,
            crc: Some(crc32(0, &asset.data)),
        };
        self.send_transfer(UPLOAD_WIDTH, &info.encode()?)?;
        self.transfer_ack(3, None)?;
        let mut report = |offset| {
            progress(UploadCheckpoint {
                file_type,
                total,
                token: start.token,
                offset,
            })
            .then_some(())
            .ok_or_else(|| invalid("Upload cancelled locally"))
        };
        report(offset)?;
        let mut committed = offset as usize;
        while committed < asset.data.len() {
            let end = (committed + stride * start.slots as usize)
                .min(asset.data.len());
            let window = UploadWindow {
                chunks: asset.data[committed..end].chunks(stride).collect(),
                committed: committed as u32,
                end: end as u32,
                last: end == asset.data.len(),
            };
            self.send_slots(&window, u32::MAX)?;
            if !self.confirm_window(&window, start.slots)? {
                return Ok(UploadOutcome::AlreadyPresent);
            }
            let mut shift = vec![5];
            shift.extend(window.end.to_le_bytes());
            self.send_transfer(UPLOAD_WIDTH, &shift)?;
            self.transfer_ack(5, Some(window.end))?;
            committed = end;
            report(window.end)?;
        }
        let result = self.wait_processing(file_type, start.token, policy)?;
        Ok(if file_type == 1 {
            UploadOutcome::Staged(result)
        } else {
            UploadOutcome::Processed(result)
        })
    }
    /// Send the slots set in `slots`, then the last window's end marker.
    fn send_slots(
        &mut self,
        window: &UploadWindow<'_>,
        slots: u32,
    ) -> Result<()> {
        for (i, chunk) in window.chunks.iter().enumerate() {
            if slots & (1 << i) != 0 {
                let payload = data_payload(UPLOAD_WIDTH, i as u8, chunk)?;
                self.send_transfer(UPLOAD_WIDTH, &payload)?;
            }
        }
        if window.last {
            let last = (window.chunks.len() - 1) as u8;
            self.send_transfer(UPLOAD_WIDTH, &[6, 128, last])?;
        }
        Ok(())
    }
    /// Wait for the device to take `window`, resending the slots it reports
    /// missing. Returns false when the device already holds the asset.
    fn confirm_window(
        &mut self,
        window: &UploadWindow<'_>,
        slots: u8,
    ) -> Result<bool> {
        let mut retries = 0;
        loop {
            let p = self
                .session
                .continuation(0x98, |f| f.payload.first() == Some(&4))?;
            let reply = WindowReply::decode(&p, slots)?;
            match reply.status {
                0x81 => {
                    if reply.committed != window.committed
                        || reply.missing & window.mask() == 0
                    {
                        return Err(invalid("Recovery offset/bitmap mismatch"));
                    }
                    if retries == UPLOAD_RETRIES {
                        return Err(unsupported(
                            "Missing-slot retry budget exhausted; reconnect before explicit resume",
                        ));
                    }
                    retries += 1;
                    self.send_slots(window, reply.missing)?;
                }
                0x10 => return Ok(false),
                0 => {
                    if reply.committed != window.end
                        || reply.missing & window.mask() != 0
                    {
                        return Err(invalid(
                            "Window completion offset/bitmap mismatch",
                        ));
                    }
                    return Ok(true);
                }
                status => {
                    return Err(invalid(format!(
                        "Transfer validator/window status {status:02x}"
                    )));
                }
            }
        }
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
                Err(e) if e.is_timeout() => {
                    if Instant::now() >= deadline {
                        return Err(e);
                    }
                    // Type01 staging sends an asynchronous success but does not
                    // populate the firmware's query cache on this path.
                    if file_type != 1 {
                        self.send_transfer(
                            FrameWidth::Short,
                            &processing_query(file_type, token),
                        )?;
                    }
                }
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Err(invalid("Processing deadline"));
            }
        }
    }
    /// Fresh multiwindow download with missing-slot recovery, at most 16 MiB.
    /// Host policy: 60 seconds overall, one second per recovery interval, ten retries.
    /// No partial data escapes. Both data frame markers are decoded.
    /// Room descriptor stale-stack regions are sanitized after CRC verification.
    pub fn download_asset(
        &mut self,
        kind: DownloadKind,
    ) -> Result<Information> {
        self.transfer(|x4| {
            let info = x4.start_download(kind)?;
            let mut data = Vec::with_capacity(info.total as usize);
            let deadline = Instant::now() + DOWNLOAD_TIMEOUT;
            while data.len() < info.total as usize {
                x4.download_window(&info, &mut data, deadline)?;
            }
            if info.crc.is_some_and(|c| crc32(0, &data) != c) {
                return Err(invalid("Download IEEE CRC32 mismatch"));
            }
            sanitize_download(kind, data)
        })
    }
    /// Negotiate a download and acknowledge its parameters.
    fn start_download(&mut self, kind: DownloadKind) -> Result<TransferInfo> {
        let file_type = kind.wire();
        let request = TransferStart::Download(kind).encode()?;
        self.send_transfer(FrameWidth::Extended, &request)?;
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
        self.send_transfer(FrameWidth::Extended, &start)?;
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
            || info.total as usize > ASSET_LIMIT
        {
            return Err(unsupported(
                "Download exceeds 16 MiB or negotiated packet bounds",
            ));
        }
        // ACK03 uses flags=1, ACK05 uses zero context, matching retained export capture.
        self.session.transfer_send(&download_ack(3, 1))?;
        Ok(info)
    }
    /// Collect one window into `data`, asking for whatever does not arrive.
    fn download_window(
        &mut self,
        info: &TransferInfo,
        data: &mut Vec<u8>,
        deadline: Instant,
    ) -> Result<()> {
        let mut window = DownloadWindow::new(info, data.len());
        let mut retries = 0;
        loop {
            let until = deadline.min(Instant::now() + DOWNLOAD_POLL);
            let received =
                self.session.continuation_frame_until(0x98, until, |f| {
                    matches!(f.payload.first(), Some(4 | 6))
                });
            let timed_out = match received {
                Ok(frame) => {
                    window.accept(&frame)?;
                    false
                }
                Err(e) if e.is_timeout() => true,
                Err(e) => return Err(e),
            };
            if Instant::now() >= deadline {
                return Err(invalid("Download deadline"));
            }
            let (needed, final_window) = window.extent();
            if window
                .last
                .is_some_and(|slot| !final_window || slot + 1 != needed)
            {
                return Err(invalid("End marker disagrees with total"));
            }
            let missing = window.missing(needed);
            let complete =
                missing == 0 && (!final_window || window.last.is_some());
            if !complete && !timed_out && window.last.is_none() {
                continue;
            }
            if !complete {
                if retries == DOWNLOAD_RETRIES {
                    return Err(invalid("Download recovery budget exhausted"));
                }
                retries += 1;
            }
            let held = window.slots[..needed].iter().flatten().map(Vec::len);
            let committed =
                data.len() + if complete { held.sum::<usize>() } else { 0 };
            let mut reply = vec![4, if complete { 0 } else { 0x81 }];
            reply.extend((missing | !slot_mask(needed as u8)).to_le_bytes());
            reply.extend((committed as u32).to_le_bytes());
            self.send_transfer(FrameWidth::Extended, &reply)?;
            if !complete {
                // A repeated marker triggers another response; ordinary recovered
                // data is collected until all required slots have arrived.
                window.last = None;
                continue;
            }
            let shift = self
                .session
                .continuation(0x98, |f| f.payload.first() == Some(&5))?;
            if shift.len() != 5 || word(&shift, 1) != committed as u32 {
                return Err(invalid("Download committed offset"));
            }
            self.session.transfer_send(&download_ack(5, 0))?;
            data.extend(
                window.slots.into_iter().take(needed).flatten().flatten(),
            );
            return Ok(());
        }
    }
}
fn push_string(p: &mut Vec<u8>, s: &[u8]) -> Result<()> {
    if s.contains(&0) {
        return Err(invalid("Metadata string contains NUL"));
    }
    p.extend(s);
    p.push(0);
    Ok(())
}
fn read_string(p: &[u8], pos: &mut usize) -> Result<Vec<u8>> {
    let n = p[*pos..]
        .iter()
        .position(|x| *x == 0)
        .ok_or_else(|| invalid("Unterminated metadata string"))?;
    let s = p[*pos..*pos + n].to_vec();
    *pos += n + 1;
    Ok(s)
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
        push_string(&mut p, &self.user_id)?;
        push_string(&mut p, &self.description)?;
        p.extend(self.timestamp.to_le_bytes());
        push_string(&mut p, &self.parent_profile_id)?;
        push_string(&mut p, &self.generation)?;
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
        let mut pos = 4;
        let user_id = read_string(p, &mut pos)?;
        let description = read_string(p, &mut pos)?;
        let timestamp = p[pos..]
            .first_chunk()
            .copied()
            .map(u64::from_le_bytes)
            .ok_or_else(|| invalid("Truncated metadata timestamp"))?;
        pos += 8;
        let parent_profile_id = read_string(p, &mut pos)?;
        let generation = read_string(p, &mut pos)?;
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
            mode: std::array::from_fn(|i| p[40 + i]),
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
