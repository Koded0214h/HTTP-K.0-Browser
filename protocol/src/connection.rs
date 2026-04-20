// src/connection.rs — K.0 connection + handshake state machine

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex};

const MAX_PACKET_SIZE: usize = 1350; // safe UDP MTU

#[derive(Debug, Clone, PartialEq)]
pub enum HandshakeState {
    Idle,
    InitSent,
    InitAckReceived,
    Done,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamState {
    Open,
    HalfClosed,
    Closed,
}

#[derive(Debug, Clone)]
pub struct Stream {
    pub id: u32,
    pub state: StreamState,
    pub ordered: bool,
    pub reliable: bool,
    pub recv_buf: Vec<Vec<u8>>,
}

impl Stream {
    pub fn new(id: u32, ordered: bool, reliable: bool) -> Self {
        Self { id, state: StreamState::Open, ordered, reliable, recv_buf: vec![] }
    }
}

#[derive(Debug)]
pub struct K0Connection {
    pub conn_id: u64,
    pub peer_addr: SocketAddr,
    pub handshake_state: HandshakeState,
    pub streams: HashMap<u32, Stream>,
    pub next_stream_id: u32,
    pub packet_num: u64,
}

impl K0Connection {
    pub fn new(conn_id: u64, peer_addr: SocketAddr) -> Self {
        Self {
            conn_id,
            peer_addr,
            handshake_state: HandshakeState::Idle,
            streams: HashMap::new(),
            next_stream_id: 0,
            packet_num: 0,
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
}