// src/packet.rs — K.0 frame codec
// Wire format:
//   [8B conn_id][8B packet_num][1B flags][1B frame_type][4B payload_len][payload...]
//
// flags byte:
//   bit 0 = ordered (1) / unordered (0)
//   bit 1 = reliable (1) / partial (0)
//   bit 2 = fec_parity (this packet is a parity shard, not data)

pub const HEADER_SIZE: usize = 22; // 8+8+1+1+4

#[derive(Debug, Clone, PartialEq)]
pub enum FrameType {
    HandshakeInit    = 0x00,
    HandshakeInitAck = 0x01,
    HandshakeDone    = 0x02,
    Stream           = 0x10,
    Ack              = 0x20,
    Nak              = 0x21,
    PathChallenge    = 0x30,
    PathResponse     = 0x31,
    FecParity        = 0x40,
    Control          = 0x50,
    Close            = 0xFF,
    Unknown          = 0xEE,
}

impl FrameType {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0x00 => Self::HandshakeInit,
            0x01 => Self::HandshakeInitAck,
            0x02 => Self::HandshakeDone,
            0x10 => Self::Stream,
            0x20 => Self::Ack,
            0x21 => Self::Nak,
            0x30 => Self::PathChallenge,
            0x31 => Self::PathResponse,
            0x40 => Self::FecParity,
            0x50 => Self::Control,
            0xFF => Self::Close,
            _    => Self::Unknown,
        }
    }

    pub fn to_byte(&self) -> u8 {
        match self {
            Self::HandshakeInit    => 0x00,
            Self::HandshakeInitAck => 0x01,
            Self::HandshakeDone    => 0x02,
            Self::Stream           => 0x10,
            Self::Ack              => 0x20,
            Self::Nak              => 0x21,
            Self::PathChallenge    => 0x30,
            Self::PathResponse     => 0x31,
            Self::FecParity        => 0x40,
            Self::Control          => 0x50,
            Self::Close            => 0xFF,
            Self::Unknown          => 0xEE,
        }
    }
}

#[derive(Debug, Clone)]
pub struct K0Packet {
    pub conn_id:    u64,
    pub packet_num: u64,
    pub flags:      u8,
    pub frame_type: FrameType,
    pub payload:    Vec<u8>,
}

impl K0Packet {
    // ── flag helpers ──────────────────────────────────────────────────────────
    pub fn is_ordered(&self)   -> bool { self.flags & 0b0000_0001 != 0 }
    pub fn is_reliable(&self)  -> bool { self.flags & 0b0000_0010 != 0 }
    pub fn is_fec_parity(&self)-> bool { self.flags & 0b0000_0100 != 0 }

    pub fn flags_for(ordered: bool, reliable: bool) -> u8 {
        (ordered as u8) | ((reliable as u8) << 1)
    }

    // ── encode ────────────────────────────────────────────────────────────────
    pub fn encode(&self) -> Vec<u8> {
        let payload_len = self.payload.len() as u32;
        let mut buf = Vec::with_capacity(HEADER_SIZE + self.payload.len());
        buf.extend_from_slice(&self.conn_id.to_be_bytes());
        buf.extend_from_slice(&self.packet_num.to_be_bytes());
        buf.push(self.flags);
        buf.push(self.frame_type.to_byte());
        buf.extend_from_slice(&payload_len.to_be_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    // ── decode ────────────────────────────────────────────────────────────────
    pub fn decode(raw: &[u8]) -> Option<Self> {
        if raw.len() < HEADER_SIZE { return None; }
        let conn_id    = u64::from_be_bytes(raw[0..8].try_into().ok()?);
        let packet_num = u64::from_be_bytes(raw[8..16].try_into().ok()?);
        let flags      = raw[16];
        let frame_type = FrameType::from_byte(raw[17]);
        let payload_len= u32::from_be_bytes(raw[18..22].try_into().ok()?) as usize;
        if raw.len() < HEADER_SIZE + payload_len { return None; }
        let payload = raw[HEADER_SIZE..HEADER_SIZE + payload_len].to_vec();
        Some(Self { conn_id, packet_num, flags, frame_type, payload })
    }

    // ── stream packet builder ─────────────────────────────────────────────────
    // payload layout for Stream frames:
    //   [4B stream_id][4B offset][data...]
    pub fn stream(
        conn_id: u64, packet_num: u64,
        stream_id: u32, offset: u32,
        data: &[u8],
        ordered: bool, reliable: bool,
    ) -> Self {
        let mut payload = Vec::with_capacity(8 + data.len());
        payload.extend_from_slice(&stream_id.to_be_bytes());
        payload.extend_from_slice(&offset.to_be_bytes());
        payload.extend_from_slice(data);
        Self {
            conn_id,
            packet_num,
            flags: Self::flags_for(ordered, reliable),
            frame_type: FrameType::Stream,
            payload,
        }
    }

    pub fn parse_stream_header(payload: &[u8]) -> Option<(u32, u32, &[u8])> {
        if payload.len() < 8 { return None; }
        let stream_id = u32::from_be_bytes(payload[0..4].try_into().ok()?);
        let offset    = u32::from_be_bytes(payload[4..8].try_into().ok()?);
        Some((stream_id, offset, &payload[8..]))
    }

    // ── ACK packet builder ────────────────────────────────────────────────────
    // ACK payload: [8B ack_packet_num][8B ack_delay_us]
    pub fn ack(conn_id: u64, packet_num: u64, acking: u64, delay_us: u64) -> Self {
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&acking.to_be_bytes());
        payload.extend_from_slice(&delay_us.to_be_bytes());
        Self {
            conn_id,
            packet_num,
            flags: 0,
            frame_type: FrameType::Ack,
            payload,
        }
    }

    pub fn parse_ack(payload: &[u8]) -> Option<(u64, u64)> {
        if payload.len() < 16 { return None; }
        let acking    = u64::from_be_bytes(payload[0..8].try_into().ok()?);
        let delay_us  = u64::from_be_bytes(payload[8..16].try_into().ok()?);
        Some((acking, delay_us))
    }

    // ── PATH_CHALLENGE / PATH_RESPONSE ────────────────────────────────────────
    pub fn path_challenge(conn_id: u64, packet_num: u64, nonce: u64) -> Self {
        Self {
            conn_id, packet_num, flags: 0,
            frame_type: FrameType::PathChallenge,
            payload: nonce.to_be_bytes().to_vec(),
        }
    }

    pub fn path_response(conn_id: u64, packet_num: u64, nonce: u64) -> Self {
        Self {
            conn_id, packet_num, flags: 0,
            frame_type: FrameType::PathResponse,
            payload: nonce.to_be_bytes().to_vec(),
        }
    }
}

// ── unit tests ────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_handshake() {
        let pkt = K0Packet {
            conn_id: 0xdeadbeef_cafebabe,
            packet_num: 42,
            flags: 0,
            frame_type: FrameType::HandshakeInit,
            payload: b"K0_INIT".to_vec(),
        };
        let encoded = pkt.encode();
        let decoded = K0Packet::decode(&encoded).unwrap();
        assert_eq!(decoded.conn_id, 0xdeadbeef_cafebabe);
        assert_eq!(decoded.packet_num, 42);
        assert_eq!(decoded.frame_type, FrameType::HandshakeInit);
        assert_eq!(decoded.payload, b"K0_INIT");
    }

    #[test]
    fn stream_flags() {
        let pkt = K0Packet {
            conn_id: 1, packet_num: 0,
            flags: K0Packet::flags_for(true, false),
            frame_type: FrameType::Stream,
            payload: vec![],
        };
        assert!(pkt.is_ordered());
        assert!(!pkt.is_reliable());
    }

    #[test]
    fn stream_header_roundtrip() {
        let pkt = K0Packet::stream(1, 5, 3, 100, b"hello world", true, true);
        let (sid, off, data) = K0Packet::parse_stream_header(&pkt.payload).unwrap();
        assert_eq!(sid, 3);
        assert_eq!(off, 100);
        assert_eq!(data, b"hello world");
    }

    #[test]
    fn ack_roundtrip() {
        let pkt = K0Packet::ack(1, 10, 9, 500);
        let (acking, delay) = K0Packet::parse_ack(&pkt.payload).unwrap();
        assert_eq!(acking, 9);
        assert_eq!(delay, 500);
    }

    #[test]
    fn rejects_short_packet() {
        assert!(K0Packet::decode(&[0u8; 5]).is_none());
    }
}