// src/connection.rs — K.0 connection state machine
// Upgraded: ACK tracking per stream, ordering buffer, FEC decoder per stream

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use crate::ack::AckTracker;
use crate::fec::FecDecoder;

const MAX_PACKET_SIZE: usize = 1350;

// ── Handshake state ───────────────────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq)]
pub enum HandshakeState {
    Idle,
    InitSent,
    InitAckReceived,
    Done,
}

// ── Stream state ──────────────────────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq)]
pub enum StreamState {
    Open,
    HalfClosed,
    Closed,
}

// ── Stream ────────────────────────────────────────────────────────────────────
#[derive(Debug)]
pub struct Stream {
    pub id:       u32,
    pub state:    StreamState,
    pub ordered:  bool,
    pub reliable: bool,

    // ordered delivery: buffer out-of-order chunks, deliver in sequence
    pub next_expected_offset: u32,
    pub reorder_buf: BTreeMap<u32, Vec<u8>>, // offset → data

    // unordered: just dump into recv_buf immediately
    pub recv_buf: Vec<Vec<u8>>,

    // FEC decoder (only used on unreliable streams)
    pub fec: Option<FecDecoder>,

    // send-side offset counter
    pub send_offset: u32,
}

impl Stream {
    pub fn new(id: u32, ordered: bool, reliable: bool) -> Self {
        let fec = if !reliable {
            Some(FecDecoder::with_defaults())
        } else {
            None
        };
        Self {
            id, state: StreamState::Open, ordered, reliable,
            next_expected_offset: 0,
            reorder_buf: BTreeMap::new(),
            recv_buf: Vec::new(),
            fec,
            send_offset: 0,
        }
    }

    // ── receive a data chunk, handle ordering ─────────────────────────────────
    pub fn receive(&mut self, offset: u32, data: Vec<u8>) {
        if self.ordered {
            if offset == self.next_expected_offset {
                // in-order: deliver immediately
                self.next_expected_offset += data.len() as u32;
                self.recv_buf.push(data);
                // flush any buffered chunks that now fit in sequence
                loop {
                    if let Some(chunk) = self.reorder_buf.remove(&self.next_expected_offset) {
                        self.next_expected_offset += chunk.len() as u32;
                        self.recv_buf.push(chunk);
                    } else {
                        break;
                    }
                }
            } else if offset > self.next_expected_offset {
                // future chunk: buffer it
                self.reorder_buf.insert(offset, data);
            }
            // past chunk (duplicate): drop
        } else {
            // unordered: just deliver
            self.recv_buf.push(data);
        }
    }

    // ── drain all received data ───────────────────────────────────────────────
    pub fn drain(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.recv_buf)
    }

    // ── next send offset + advance ────────────────────────────────────────────
    pub fn take_offset(&mut self, len: usize) -> u32 {
        let off = self.send_offset;
        self.send_offset += len as u32;
        off
    }
}

// ── Connection ────────────────────────────────────────────────────────────────
#[derive(Debug)]
pub struct K0Connection {
    pub conn_id:          u64,
    pub peer_addr:        SocketAddr,
    pub handshake_state:  HandshakeState,
    pub streams:          HashMap<u32, Stream>,
    pub next_stream_id:   u32,
    pub packet_num:       u64,

    // ACK tracker (shared across all reliable streams on this connection)
    pub ack_tracker:      AckTracker,

    // path challenge nonce pending verification
    pub pending_challenge: Option<u64>,
}

impl K0Connection {
    pub fn new(conn_id: u64, peer_addr: SocketAddr) -> Self {
        Self {
            conn_id,
            peer_addr,
            handshake_state:   HandshakeState::Idle,
            streams:           HashMap::new(),
            next_stream_id:    0,
            packet_num:        0,
            ack_tracker:       AckTracker::new(),
            pending_challenge: None,
        }
    }

    pub fn open_stream(&mut self, ordered: bool, reliable: bool) -> u32 {
        let id = self.next_stream_id;
        self.streams.insert(id, Stream::new(id, ordered, reliable));
        self.next_stream_id += 1;
        id
    }

    pub fn close_stream(&mut self, stream_id: u32) {
        if let Some(s) = self.streams.get_mut(&stream_id) {
            s.state = StreamState::Closed;
        }
    }

    pub fn next_packet_num(&mut self) -> u64 {
        let n = self.packet_num;
        self.packet_num += 1;
        n
    }

    pub fn is_ready(&self) -> bool {
        self.handshake_state == HandshakeState::Done
    }

    // ── stats ─────────────────────────────────────────────────────────────────
    pub fn srtt_ms(&self)        -> u64 { self.ack_tracker.srtt_ms() }
    pub fn rto_ms(&self)         -> u64 { self.ack_tracker.rto_ms() }
    pub fn in_flight_count(&self)-> usize { self.ack_tracker.in_flight_count() }
}