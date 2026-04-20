// src/transport.rs — async UDP server + handshake driver

use crate::connection::{HandshakeState, K0Connection};
use crate::packet::{FrameType, K0Packet};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

pub struct K0Transport {
    socket: Arc<UdpSocket>,
    connections: Arc<Mutex<HashMap<u64, K0Connection>>>,
}

impl K0Transport {
    pub async fn bind(addr: &str) -> anyhow::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        println!("[K0] Listening on {addr}");
        Ok(Self { socket, connections: Arc::new(Mutex::new(HashMap::new())) })
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let mut buf = vec![0u8; 1500];
        loop {
            let (len, peer) = self.socket.recv_from(&mut buf).await?;
            let raw = &buf[..len];
            if let Some(pkt) = K0Packet::decode(raw) {
                self.handle_packet(pkt, peer).await;
            }
        }
    }

    async fn handle_packet(&self, pkt: K0Packet, peer: SocketAddr) {
        let mut conns = self.connections.lock().await;

        match pkt.frame_type {
            FrameType::HandshakeInit => {
                println!("[K0] INIT from {peer} conn_id={:#x}", pkt.conn_id);
                let conn = conns
                    .entry(pkt.conn_id)
                    .or_insert_with(|| K0Connection::new(pkt.conn_id, peer));
                conn.handshake_state = HandshakeState::InitSent;

                // Reply: INIT_ACK
                let ack = K0Packet {
                    conn_id: pkt.conn_id,
                    packet_num: conn.next_packet_num(),
                    flags: 0,
                    frame_type: FrameType::HandshakeInitAck,
                    payload: b"K0_INIT_ACK".to_vec(),
                };
                let _ = self.socket.send_to(&ack.encode(), peer).await;
                println!("[K0] INIT_ACK sent to {peer}");
            }

            FrameType::HandshakeDone => {
                if let Some(conn) = conns.get_mut(&pkt.conn_id) {
                    conn.handshake_state = HandshakeState::Done;
                    println!("[K0] Handshake DONE with {peer} ✓");
                    // Open default streams: 1 reliable-ordered, 1 partial-unordered
                    let s1 = conn.open_stream(true, true);
                    let s2 = conn.open_stream(false, false);
                    println!("[K0] Streams opened: reliable={s1}, partial={s2}");
                }
            }

            FrameType::Stream => {
                if let Some(conn) = conns.get_mut(&pkt.conn_id) {
                    if conn.handshake_state != HandshakeState::Done {
                        eprintln!("[K0] Stream data before handshake — dropping");
                        return;
                    }
                    let stream_id = u32::from_be_bytes(
                        pkt.payload.get(0..4).and_then(|b| b.try_into().ok()).unwrap_or([0;4])
                    );
                    let data = &pkt.payload[4..];
                    println!("[K0] stream={stream_id} {} bytes — ordered={} reliable={}",
                        data.len(), pkt.is_ordered(), pkt.is_reliable());
                    if let Some(stream) = conn.streams.get_mut(&stream_id) {
                        stream.recv_buf.push(data.to_vec());
                    }
                }
            }

            FrameType::Ack => {
                println!("[K0] ACK from {peer} pkt={}", pkt.packet_num);
            }

            _ => {
                println!("[K0] Unhandled frame {:?} from {peer}", pkt.frame_type);
            }
        }
    }

    pub async fn connect(&self, peer: SocketAddr, conn_id: u64) -> anyhow::Result<()> {
        let init = K0Packet {
            conn_id,
            packet_num: 0,
            flags: 0,
            frame_type: FrameType::HandshakeInit,
            payload: b"K0_INIT".to_vec(),
        };
        self.socket.send_to(&init.encode(), peer).await?;
        println!("[K0] INIT sent to {peer}");

        // Wait for INIT_ACK
        let mut buf = vec![0u8; 1500];
        let (len, _) = self.socket.recv_from(&mut buf).await?;
        let reply = K0Packet::decode(&buf[..len]).ok_or(anyhow::anyhow!("bad packet"))?;
        if reply.frame_type != FrameType::HandshakeInitAck {
            return Err(anyhow::anyhow!("expected INIT_ACK"));
        }
        println!("[K0] INIT_ACK received ✓");

        // Send HANDSHAKE_DONE
        let done = K0Packet {
            conn_id,
            packet_num: 1,
            flags: 0,
            frame_type: FrameType::HandshakeDone,
            payload: b"K0_DONE".to_vec(),
        };
        self.socket.send_to(&done.encode(), peer).await?;
        println!("[K0] Handshake complete ✓");
        Ok(())
    }
}