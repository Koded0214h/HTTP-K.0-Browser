#![allow(dead_code)]
// src/frame.rs — HTTP/K.0 application framing
//
// Sits ON TOP of the transport layer streams.
// Transport handles: bytes in, bytes out, reliability, FEC.
// This layer handles: what those bytes MEAN as HTTP requests/responses.
//
// Wire format for a K.0 HTTP frame:
//   [1B frame_kind][4B payload_len][payload...]
//
// Frame kinds:
//   0x01 HEADERS  — request/response headers (compressed)
//   0x02 DATA     — body chunk
//   0x03 TRAILERS — trailing headers (after body)
//   0x04 RESET    — cancel this request stream
//   0x05 SETTINGS — connection-level config
//   0x06 PUSH     — server push (future)
//   0x07 GOAWAY   — graceful shutdown
//   0x08 PING     — keepalive
//
// A full HTTP exchange on stream N looks like:
//   Client → [HEADERS: GET /index.html][DATA: <empty>]
//   Server → [HEADERS: 200 OK][DATA: <html...>][DATA: ...][TRAILERS]

use bytes::{Buf, BufMut, Bytes, BytesMut};

// ── Frame kind byte ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum FrameKind {
    Headers  = 0x01,
    Data     = 0x02,
    Trailers = 0x03,
    Reset    = 0x04,
    Settings = 0x05,
    Push     = 0x06,
    GoAway   = 0x07,
    Ping     = 0x08,
    Unknown  = 0xFF,
}

impl FrameKind {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0x01 => Self::Headers,
            0x02 => Self::Data,
            0x03 => Self::Trailers,
            0x04 => Self::Reset,
            0x05 => Self::Settings,
            0x06 => Self::Push,
            0x07 => Self::GoAway,
            0x08 => Self::Ping,
            _    => Self::Unknown,
        }
    }
    pub fn to_byte(&self) -> u8 {
        match self {
            Self::Headers  => 0x01,
            Self::Data     => 0x02,
            Self::Trailers => 0x03,
            Self::Reset    => 0x04,
            Self::Settings => 0x05,
            Self::Push     => 0x06,
            Self::GoAway   => 0x07,
            Self::Ping     => 0x08,
            Self::Unknown  => 0xFF,
        }
    }
}

// ── HTTP/K.0 application frame ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct K0Frame {
    pub kind:    FrameKind,
    pub payload: Bytes,
}

impl K0Frame {
    pub fn new(kind: FrameKind, payload: impl Into<Bytes>) -> Self {
        Self { kind, payload: payload.into() }
    }

    // ── encode: [1B kind][4B len][payload] ────────────────────────────────────
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(5 + self.payload.len());
        buf.put_u8(self.kind.to_byte());
        buf.put_u32(self.payload.len() as u32);
        buf.put_slice(&self.payload);
        buf.freeze()
    }

    // ── decode from a byte slice ──────────────────────────────────────────────
    pub fn decode(src: &mut BytesMut) -> Option<Self> {
        if src.len() < 5 { return None; }
        let len = u32::from_be_bytes(src[1..5].try_into().ok()?) as usize;
        if src.len() < 5 + len { return None; }
        let kind    = FrameKind::from_byte(src[0]);
        src.advance(5);
        let payload = src.split_to(len).freeze();
        Some(Self { kind, payload })
    }

    // ── frame constructors ────────────────────────────────────────────────────

    pub fn headers(encoded_headers: Bytes) -> Self {
        Self::new(FrameKind::Headers, encoded_headers)
    }

    pub fn data(body_chunk: impl Into<Bytes>) -> Self {
        Self::new(FrameKind::Data, body_chunk.into())
    }

    pub fn trailers(encoded: Bytes) -> Self {
        Self::new(FrameKind::Trailers, encoded)
    }

    pub fn reset(error_code: u32) -> Self {
        Self::new(FrameKind::Reset, error_code.to_be_bytes().to_vec())
    }

    pub fn ping(token: u64) -> Self {
        Self::new(FrameKind::Ping, token.to_be_bytes().to_vec())
    }

    pub fn go_away(last_stream_id: u32, error_code: u32) -> Self {
        let mut p = Vec::with_capacity(8);
        p.extend_from_slice(&last_stream_id.to_be_bytes());
        p.extend_from_slice(&error_code.to_be_bytes());
        Self::new(FrameKind::GoAway, p)
    }
}

// ── Frame stream decoder ──────────────────────────────────────────────────────
// Sits on top of a transport stream's recv_buf.
// Accumulates raw bytes, decodes complete frames as they arrive.
// Handles fragmentation — a frame might arrive across multiple packets.

pub struct FrameDecoder {
    buf: BytesMut,
}

impl FrameDecoder {
    pub fn new() -> Self { Self { buf: BytesMut::new() } }

    /// Feed raw bytes from the transport stream into the decoder.
    pub fn feed(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Try to decode the next complete frame. Returns None if more bytes needed.
    pub fn next_frame(&mut self) -> Option<K0Frame> {
        K0Frame::decode(&mut self.buf)
    }

    /// Drain all currently decodable frames.
    pub fn drain_frames(&mut self) -> Vec<K0Frame> {
        let mut frames = Vec::new();
        while let Some(f) = self.next_frame() {
            frames.push(f);
        }
        frames
    }

    pub fn buffered_bytes(&self) -> usize { self.buf.len() }
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_data_frame() {
        let f = K0Frame::data(b"hello world".as_ref());
        let encoded = f.encode();
        let mut buf = BytesMut::from(encoded.as_ref());
        let decoded = K0Frame::decode(&mut buf).unwrap();
        assert_eq!(decoded.kind, FrameKind::Data);
        assert_eq!(decoded.payload.as_ref(), b"hello world");
    }

    #[test]
    fn partial_frame_returns_none() {
        let f = K0Frame::data(b"some data".as_ref());
        let encoded = f.encode();
        // only feed first 4 bytes — not enough for the header yet
        let mut buf = BytesMut::from(&encoded[..4]);
        assert!(K0Frame::decode(&mut buf).is_none());
    }

    #[test]
    fn multi_frame_drain() {
        let mut dec = FrameDecoder::new();
        let f1 = K0Frame::data(b"chunk1".as_ref());
        let f2 = K0Frame::ping(0xdeadbeef);
        dec.feed(&f1.encode());
        dec.feed(&f2.encode());
        let frames = dec.drain_frames();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].kind, FrameKind::Data);
        assert_eq!(frames[1].kind, FrameKind::Ping);
    }

    #[test]
    fn frame_decoder_handles_fragmentation() {
        let f = K0Frame::data(b"fragmented payload".as_ref());
        let encoded = f.encode();
        let mut dec = FrameDecoder::new();
        // feed byte by byte
        for byte in encoded.iter() {
            dec.feed(&[*byte]);
        }
        let frames = dec.drain_frames();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload.as_ref(), b"fragmented payload");
    }

    #[test]
    fn reset_frame_carries_error_code() {
        let f = K0Frame::reset(404);
        let encoded = f.encode();
        let mut buf = BytesMut::from(encoded.as_ref());
        let decoded = K0Frame::decode(&mut buf).unwrap();
        assert_eq!(decoded.kind, FrameKind::Reset);
        let code = u32::from_be_bytes(decoded.payload[0..4].try_into().unwrap());
        assert_eq!(code, 404);
    }
}