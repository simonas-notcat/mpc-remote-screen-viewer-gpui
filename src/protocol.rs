use std::fmt;

pub const MAX_PAYLOAD: usize = 64 * 1024 * 1024;
pub const VIDEO_CODECS: u8 = 0x1f;
pub const AUDIO_CODECS: u8 = 0x07;
pub const DEFAULT_PORT: u16 = 7447;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("Malformed protocol data: {0}")]
    Invalid(&'static str),
    #[error("Unsupported protocol version {0}; expected 2")]
    Version(u16),
    #[error("Message payload exceeds 64 MiB")]
    Oversized,
    #[error("Message assembler is poisoned; reset the session")]
    Poisoned,
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Hello = 1,
    Auth,
    AuthResult,
    ServerInit,
    FrameUpdate,
    Ack,
    RequestFullRefresh,
    SetOptions,
    Ping,
    Pong,
    Bye,
    Input,
    AudioControl,
    AudioFormat,
    AudioPacket,
    DeviceInfo,
}
impl TryFrom<u8> for Kind {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self> {
        Ok(match value {
            1 => Self::Hello,
            2 => Self::Auth,
            3 => Self::AuthResult,
            4 => Self::ServerInit,
            5 => Self::FrameUpdate,
            6 => Self::Ack,
            7 => Self::RequestFullRefresh,
            8 => Self::SetOptions,
            9 => Self::Ping,
            10 => Self::Pong,
            11 => Self::Bye,
            12 => Self::Input,
            13 => Self::AudioControl,
            14 => Self::AudioFormat,
            15 => Self::AudioPacket,
            16 => Self::DeviceInfo,
            _ => return Err(Error::Invalid("message type outside 1..=16")),
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: Kind,
    pub payload: Vec<u8>,
}
// Payloads can contain authentication material. Never include them in diagnostics.
impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Message")
            .field("kind", &self.kind)
            .field("payload_length", &self.payload.len())
            .finish()
    }
}
impl Message {
    pub fn new(kind: Kind, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            kind,
            payload: payload.into(),
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(Error::Oversized);
        }
        let mut out = Vec::with_capacity(5 + self.payload.len());
        out.push(self.kind as u8);
        out.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }
}

/// Retains only the incomplete message, allocating in proportion to received data,
/// never to an untrusted advertised length. Any invalid envelope poisons this instance.
#[derive(Default)]
pub struct Assembler {
    header: [u8; 5],
    header_len: usize,
    expected: usize,
    payload: Vec<u8>,
    poisoned: bool,
}
impl Assembler {
    pub fn feed(&mut self, mut bytes: &[u8]) -> Result<Vec<Message>> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let mut messages = Vec::new();
        while !bytes.is_empty() {
            if self.header_len < 5 {
                let n = (5 - self.header_len).min(bytes.len());
                self.header[self.header_len..self.header_len + n].copy_from_slice(&bytes[..n]);
                self.header_len += n;
                bytes = &bytes[n..];
                if Kind::try_from(self.header[0]).is_err() {
                    self.poisoned = true;
                    return Err(Error::Invalid("message type outside 1..=16"));
                }
                if self.header_len < 5 {
                    continue;
                }
                self.expected = u32::from_le_bytes(self.header[1..5].try_into().unwrap()) as usize;
                if self.expected > MAX_PAYLOAD {
                    self.poisoned = true;
                    return Err(Error::Oversized);
                }
            }
            let n = (self.expected - self.payload.len()).min(bytes.len());
            self.payload.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if self.payload.len() == self.expected {
                messages.push(Message::new(
                    Kind::try_from(self.header[0])?,
                    std::mem::take(&mut self.payload),
                ));
                self.header_len = 0;
                self.expected = 0;
            }
        }
        Ok(messages)
    }
    pub fn buffered_bytes(&self) -> usize {
        self.header_len + self.payload.len()
    }
}

pub(crate) struct Reader<'a> {
    remaining: &'a [u8],
}
impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }
    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining.len() {
            return Err(Error::Invalid("truncated payload"));
        }
        let (value, rest) = self.remaining.split_at(n);
        self.remaining = rest;
        Ok(value)
    }
    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub(crate) fn string(&mut self) -> Result<String> {
        let n = self.u8()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    pub(crate) fn rest(&self) -> &'a [u8] {
        self.remaining
    }
}
pub fn put_short_string(out: &mut Vec<u8>, text: &str) {
    let bytes = text.as_bytes();
    let n = bytes.len().min(255);
    out.push(n as u8);
    out.extend_from_slice(&bytes[..n]);
}
pub fn parse_reason(payload: &[u8]) -> Result<String> {
    Reader::new(payload).string()
}
pub fn parse_token(payload: &[u8]) -> Result<u64> {
    Reader::new(payload).u64()
}
pub fn token(kind: Kind, value: u64) -> Message {
    Message::new(kind, value.to_le_bytes())
}
pub fn ack(frame_id: u32) -> Message {
    Message::new(Kind::Ack, frame_id.to_le_bytes())
}
pub fn options(fps: u8) -> Message {
    Message::new(Kind::SetOptions, [fps.min(120), VIDEO_CODECS])
}
pub fn audio_control(enabled: bool) -> Message {
    Message::new(Kind::AudioControl, [u8::from(enabled), AUDIO_CODECS])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub flags: u16,
    pub nonce: [u8; 16],
}
impl Hello {
    pub fn parse(payload: &[u8]) -> Result<Self> {
        let mut r = Reader::new(payload);
        if r.take(4)? != b"RSCR" {
            return Err(Error::Invalid("Hello magic; wrong service?"));
        }
        let version = r.u16()?;
        if version != 2 {
            return Err(Error::Version(version));
        }
        Ok(Self {
            flags: r.u16()?,
            nonce: r.take(16)?.try_into().unwrap(),
        })
    }
}

#[derive(PartialEq, Eq)]
pub struct Auth {
    pub nonce: [u8; 16],
    pub proof: [u8; 32],
    pub codec_mask: u8,
    /// Client sends 0x01; its server-side meaning is not yet verified.
    pub extension: u8,
}
impl Auth {
    pub fn parse(payload: &[u8]) -> Result<Self> {
        let mut r = Reader::new(payload);
        Ok(Self {
            nonce: r.take(16)?.try_into().unwrap(),
            proof: r.take(32)?.try_into().unwrap(),
            codec_mask: if r.rest().is_empty() {
                VIDEO_CODECS
            } else {
                r.u8()?
            },
            extension: if r.rest().is_empty() { 0 } else { r.u8()? },
        })
    }
    pub fn message(&self) -> Message {
        let mut out = Vec::with_capacity(50);
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.proof);
        out.extend_from_slice(&[self.codec_mask, self.extension]);
        Message::new(Kind::Auth, out)
    }
}
pub enum AuthResult {
    Success([u8; 32]),
    Rejected(String),
}
impl AuthResult {
    pub fn parse(payload: &[u8]) -> Result<Self> {
        let mut r = Reader::new(payload);
        if r.u8()? != 0 {
            Ok(Self::Success(r.take(32)?.try_into().unwrap()))
        } else {
            Ok(Self::Rejected(r.string()?))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelFormat(pub [u8; 7]);
impl PixelFormat {
    pub fn validate(self) -> Result<Self> {
        let [bpp, rs, rb, gs, gb, bs, bb] = self.0;
        if ![16, 24, 32].contains(&bpp) {
            return Err(Error::Invalid("pixel bit depth"));
        }
        for (shift, bits) in [(rs, rb), (gs, gb), (bs, bb)] {
            if !(1..=8).contains(&bits) || u16::from(shift) + u16::from(bits) > u16::from(bpp) {
                return Err(Error::Invalid("pixel channel mask"));
            }
        }
        Ok(self)
    }
    pub fn bytes_per_pixel(self) -> usize {
        usize::from(self.0[0] / 8)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInit {
    pub width: u16,
    pub height: u16,
    pub pixel_format: PixelFormat,
    pub name: String,
    pub rotation: u8,
    pub input_capabilities: u8,
    pub audio_available: bool,
}
impl ServerInit {
    pub fn parse(payload: &[u8]) -> Result<Self> {
        let mut r = Reader::new(payload);
        let width = r.u16()?;
        let height = r.u16()?;
        if width == 0 || height == 0 {
            return Err(Error::Invalid("empty screen dimensions"));
        }
        let pixel_format = PixelFormat(r.take(7)?.try_into().unwrap()).validate()?;
        let name = r.string()?;
        let rotation = r.u8()?;
        if rotation > 3 {
            return Err(Error::Invalid("screen rotation"));
        }
        let input_capabilities = r.u8()?;
        let audio_available = !r.rest().is_empty() && r.u8()? != 0;
        Ok(Self {
            width,
            height,
            pixel_format,
            name,
            rotation,
            input_capabilities,
            audio_available,
        })
    }
    pub fn upright_size(&self) -> (u16, u16) {
        if self.rotation.is_multiple_of(2) {
            (self.width, self.height)
        } else {
            (self.height, self.width)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}
#[derive(Debug)]
pub struct Rectangle<'a> {
    pub bounds: Bounds,
    pub codec: u8,
    pub drop_bits: u8,
    pub encoded: &'a [u8],
}
#[derive(Debug)]
pub struct FrameUpdate<'a> {
    pub id: u32,
    pub quality: u8,
    pub rectangles: Vec<Rectangle<'a>>,
}
impl<'a> FrameUpdate<'a> {
    pub fn parse(payload: &'a [u8]) -> Result<Self> {
        let mut r = Reader::new(payload);
        let id = r.u32()?;
        let quality = r.u8()?;
        let count = r.u16()? as usize;
        if count > r.rest().len() / 14 {
            return Err(Error::Invalid("Corrupt screen update"));
        }
        let mut rectangles = Vec::with_capacity(count);
        for _ in 0..count {
            let bounds = Bounds {
                x: r.u16()?,
                y: r.u16()?,
                width: r.u16()?,
                height: r.u16()?,
            };
            let codec = r.u8()?;
            let drop_bits = r.u8()?;
            let length = r.u32()? as usize;
            rectangles.push(Rectangle {
                bounds,
                codec,
                drop_bits,
                encoded: r.take(length)?,
            });
        }
        Ok(Self {
            id,
            quality,
            rectangles,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub model: String,
    pub serial: String,
    pub firmware: String,
}
impl DeviceInfo {
    pub fn parse(payload: &[u8]) -> Result<Self> {
        let mut r = Reader::new(payload);
        Ok(Self {
            model: r.string()?,
            serial: r.string()?,
            firmware: r.string()?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEvent {
    pub kind: u8,
    pub id: u8,
    pub x_or_key: u16,
    pub y: u16,
    pub value: i16,
}
impl InputEvent {
    pub const RELEASE_ALL: Self = Self {
        kind: 7,
        id: 0,
        x_or_key: 0,
        y: 0,
        value: 0,
    };
    pub fn parse_batch(payload: &[u8]) -> Result<Vec<Self>> {
        let mut r = Reader::new(payload);
        let count = r.u8()? as usize;
        if count > 64 {
            return Err(Error::Invalid("input batch exceeds 64 events"));
        }
        (0..count)
            .map(|_| {
                let event = Self {
                    kind: r.u8()?,
                    id: r.u8()?,
                    x_or_key: r.u16()?,
                    y: r.u16()?,
                    value: r.u16()? as i16,
                };
                event.validate()?;
                Ok(event)
            })
            .collect()
    }
    fn validate(self) -> Result<()> {
        if self.kind >= 10 {
            return Err(Error::Invalid("input event kind"));
        }
        if self.kind <= 2 && self.id >= 10 {
            return Err(Error::Invalid("touch contact ID"));
        }
        Ok(())
    }
    /// Split rather than truncate transitions when more than 64 events are queued.
    pub fn messages(events: &[Self]) -> Result<Vec<Message>> {
        for event in events {
            event.validate()?;
        }
        Ok(events
            .chunks(64)
            .map(|batch| {
                let mut out = Vec::with_capacity(1 + batch.len() * 8);
                out.push(batch.len() as u8);
                for event in batch {
                    out.extend_from_slice(&[event.kind, event.id]);
                    out.extend_from_slice(&event.x_or_key.to_le_bytes());
                    out.extend_from_slice(&event.y.to_le_bytes());
                    out.extend_from_slice(&event.value.to_le_bytes());
                }
                Message::new(Kind::Input, out)
            })
            .collect())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u8,
    pub codec: u8,
}
impl AudioFormat {
    pub fn parse(payload: &[u8]) -> Result<Self> {
        if payload.len() != 6 {
            return Err(Error::Invalid("AudioFormat must be exactly 6 bytes"));
        }
        let mut r = Reader::new(payload);
        Self {
            sample_rate: r.u32()?,
            channels: r.u8()?,
            codec: r.u8()?,
        }
        .validate()
    }
    pub fn validate(self) -> Result<Self> {
        if !(8000..=384000).contains(&self.sample_rate)
            || !(1..=8).contains(&self.channels)
            || self.codec > 2
        {
            return Err(Error::Invalid("audio sample rate, channels or codec"));
        }
        Ok(self)
    }
}
#[derive(Debug)]
pub struct AudioPacket<'a> {
    pub first_sample_index: u64,
    pub frames: u16,
    pub encoded: &'a [u8],
}
impl<'a> AudioPacket<'a> {
    pub fn parse(payload: &'a [u8], format: AudioFormat) -> Result<Self> {
        let mut r = Reader::new(payload);
        let packet = Self {
            first_sample_index: r.u64()?,
            frames: r.u16()?,
            encoded: r.rest(),
        };
        packet.validate(format)?;
        Ok(packet)
    }
    pub fn validate(&self, format: AudioFormat) -> Result<()> {
        format.validate()?;
        let Self {
            first_sample_index,
            frames,
            encoded,
        } = *self;
        if !(1..=8192).contains(&frames) {
            return Err(Error::Invalid("audio packet frame count"));
        }
        first_sample_index
            .checked_add(u64::from(frames))
            .ok_or(Error::Invalid("audio source index overflow"))?;
        let c = usize::from(format.channels);
        let f = usize::from(frames);
        let valid = match format.codec {
            0 => encoded.len() == 2 * f * c,
            1 => encoded.len() == 4 * c + ((f - 1) * c).div_ceil(2),
            2 => (3 * c..=(2 * f + 3) * c).contains(&encoded.len()),
            _ => false,
        };
        if !valid {
            return Err(Error::Invalid("encoded audio packet length"));
        }
        Ok(())
    }
}
