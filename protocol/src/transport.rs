// src/transport.rs — K.0 async transport (full)

#![allow(dead_code)]

use crate::congestion::BbrController;
use crate::connection::{HandshakeState, K0Connection};
use crate::fec::FecEncoder;
use crate::packet::{FrameType, K0Packet};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time;

const TICK_INTERVAL_MS: u64 = 10;

pub struct K0Transport {
    socket:      Arc<UdpSocket>,
    connections: Arc<Mutex<HashMap<u64, ConnectionCtx>>>,
}

struct ConnectionCtx {
    conn:     K0Connection,
    bbr:      BbrController,
    fec_enc:  HashMap<u32, FecEncoder>,
    fec_slot: HashMap<u32, usize>, // recv-side slot counter per stream
}

impl ConnectionCtx {
    fn new(conn: K0Connection) -> Self {
        Self { conn, bbr: BbrController::new(), fec_enc: HashMap::new(), fec_slot: HashMap::new() }
    }

    fn fec_for(&mut self, stream_id: u32) -> &mut FecEncoder {
        self.fec_enc.entry(stream_id).or_insert_with(FecEncoder::with_defaults)
    }

    fn next_fec_slot(&mut self, stream_id: u32) -> usize {
        let slot = self.fec_slot.entry(stream_id).or_insert(0);
        let cur = *slot;
        *slot = (*slot + 1) % 4;
        cur
    }

    fn reset_fec_slot(&mut self, stream_id: u32) {
        self.fec_slot.insert(stream_id, 0);
    }
}

impl K0Transport {
    pub async fn bind(addr: &str) -> anyhow::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        println!("[K0] Bound on {addr}");
        Ok(Self { socket, connections: Arc::new(Mutex::new(HashMap::new())) })
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        Self::spawn_ticker(Arc::clone(&self.connections), Arc::clone(&self.socket));
        self.recv_loop().await
    }

    async fn recv_loop(&self) -> anyhow::Result<()> {
        let mut buf = vec![0u8; 1500];
        loop {
            let (len, peer) = self.socket.recv_from(&mut buf).await?;
            if let Some(pkt) = K0Packet::decode(&buf[..len]) {
                self.handle_packet(pkt, peer).await;
            } else {
                eprintln!("[K0] Bad packet from {peer} — dropped");
            }
        }
    }

    fn spawn_ticker(conns: Arc<Mutex<HashMap<u64, ConnectionCtx>>>, socket: Arc<UdpSocket>) {
        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_millis(TICK_INTERVAL_MS));
            loop {
                interval.tick().await;
                let mut map = conns.lock().await;
                for ctx in map.values_mut() {
                    ctx.conn.ack_tracker.tick();
                    while let Some(raw) = ctx.conn.ack_tracker.next_retransmit() {
                        let peer = ctx.conn.peer_addr;
                        let _ = socket.send_to(&raw, peer).await;
                    }
                }
            }
        });
    }

    async fn handle_packet(&self, pkt: K0Packet, peer: SocketAddr) {
        let mut map = self.connections.lock().await;

        match pkt.frame_type {

            FrameType::HandshakeInit => {
                println!("[K0] INIT from {peer}  conn={:#x}", pkt.conn_id);
                let ctx = map.entry(pkt.conn_id)
                    .or_insert_with(|| ConnectionCtx::new(K0Connection::new(pkt.conn_id, peer)));
                ctx.conn.handshake_state = HandshakeState::InitSent;
                let pnum = ctx.conn.next_packet_num();
                let ack = K0Packet {
                    conn_id: pkt.conn_id, packet_num: pnum, flags: 0,
                    frame_type: FrameType::HandshakeInitAck,
                    payload: b"K0_INIT_ACK".to_vec(),
                };
                let _ = self.socket.send_to(&ack.encode(), peer).await;
                println!("[K0] INIT_ACK → {peer}");
            }

            FrameType::HandshakeDone => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    ctx.conn.handshake_state = HandshakeState::Done;
                    let s1 = ctx.conn.open_stream(true,  true);
                    let s2 = ctx.conn.open_stream(false, false);
                    println!("[K0] ✓ Handshake done {peer} | reliable={s1} partial={s2}");
                }
            }

            FrameType::Stream => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    if !ctx.conn.is_ready() {
                        eprintln!("[K0] Stream before handshake — dropped");
                        return;
                    }
                    if let Some((stream_id, offset, data)) =
                        K0Packet::parse_stream_header(&pkt.payload)
                    {
                        let reliable = pkt.is_reliable();
                        let ordered  = pkt.is_ordered();

                        // feed FEC decoder slot BEFORE writing to recv_buf
                        if !reliable {
                            let slot = ctx.next_fec_slot(stream_id);
                            if let Some(stream) = ctx.conn.streams.get_mut(&stream_id) {
                                if let Some(fec) = &mut stream.fec {
                                    fec.receive_data(slot, data.to_vec());
                                }
                            }
                        }

                        if let Some(stream) = ctx.conn.streams.get_mut(&stream_id) {
                            println!(
                                "[K0] stream={stream_id} off={offset} {}B  ordered={ordered} reliable={reliable}",
                                data.len()
                            );
                            stream.receive(offset, data.to_vec());
                            if reliable {
                                ctx.conn.ack_tracker.queue_ack(pkt.packet_num);
                            }
                        } else {
                            eprintln!("[K0] Unknown stream_id={stream_id}");
                        }

                        let pending = ctx.conn.ack_tracker.drain_acks();
                        for ack_pkt_num in pending {
                            let pnum = ctx.conn.next_packet_num();
                            let ack  = K0Packet::ack(pkt.conn_id, pnum, ack_pkt_num, 0);
                            let _ = self.socket.send_to(&ack.encode(), peer).await;
                        }
                    }
                }
            }

            FrameType::Ack => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    if let Some((acked, delay_us)) = K0Packet::parse_ack(&pkt.payload) {
                        ctx.conn.ack_tracker.on_ack(acked, delay_us);
                        let rtt = ctx.conn.ack_tracker.smoothed_rtt;
                        ctx.bbr.on_ack(1350, rtt);
                        println!("[K0] ACK pkt={acked}  srtt={}ms  cwnd={}", ctx.conn.srtt_ms(), ctx.bbr.cwnd);
                    }
                }
            }

            FrameType::Nak => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    if pkt.payload.len() >= 8 {
                        let lost = u64::from_be_bytes(pkt.payload[0..8].try_into().unwrap());
                        ctx.conn.ack_tracker.on_nak(lost);
                        ctx.bbr.on_loss();
                        println!("[K0] NAK lost={lost} — queued retransmit");
                    }
                }
            }

            FrameType::FecParity => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    if pkt.payload.len() >= 4 {
                        let stream_id = u32::from_be_bytes(pkt.payload[0..4].try_into().unwrap());
                        let parity_data = pkt.payload[4..].to_vec();
                        ctx.reset_fec_slot(stream_id); // block done, reset slot counter

                        if let Some(stream) = ctx.conn.streams.get_mut(&stream_id) {
                            if let Some(fec) = &mut stream.fec {
                                fec.receive_parity(parity_data);
                                match fec.recover() {
                                    crate::fec::FecResult::Recovered { slot, data } => {
                                        println!("[K0] FEC ✓ recovered stream={stream_id} slot={slot} {}B", data.len());
                                        stream.recv_buf.push(data);
                                        fec.reset();
                                    }
                                    crate::fec::FecResult::Complete => {
                                        println!("[K0] FEC block complete stream={stream_id}");
                                        fec.reset();
                                    }
                                    crate::fec::FecResult::Unrecoverable => {
                                        eprintln!("[K0] FEC unrecoverable stream={stream_id} (>1 loss in block)");
                                        fec.reset();
                                    }
                                }
                            }
                        }
                    }
                }
            }

            FrameType::PathChallenge => {
                if pkt.payload.len() >= 8 {
                    let nonce = u64::from_be_bytes(pkt.payload[0..8].try_into().unwrap());
                    println!("[K0] PATH_CHALLENGE nonce={nonce:#x} from {peer}");
                    if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                        let pnum = ctx.conn.next_packet_num();
                        let resp = K0Packet::path_response(pkt.conn_id, pnum, nonce);
                        let _ = self.socket.send_to(&resp.encode(), peer).await;
                        println!("[K0] PATH_RESPONSE → {peer}");
                    }
                }
            }

            FrameType::PathResponse => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    if pkt.payload.len() >= 8 {
                        let nonce = u64::from_be_bytes(pkt.payload[0..8].try_into().unwrap());
                        if ctx.conn.pending_challenge == Some(nonce) {
                            ctx.conn.pending_challenge = None;
                            ctx.conn.peer_addr = peer;
                            println!("[K0] ✓ Path verified — migrated to {peer}");
                        } else {
                            eprintln!("[K0] PATH_RESPONSE nonce mismatch — dropped");
                        }
                    }
                }
            }

            FrameType::Close => {
                map.remove(&pkt.conn_id);
                println!("[K0] Connection {:#x} closed by {peer}", pkt.conn_id);
            }

            _ => println!("[K0] Unhandled {:?} from {peer}", pkt.frame_type),
        }
    }

    // ── CLIENT API ────────────────────────────────────────────────────────────

    pub async fn connect(&self, peer: SocketAddr, conn_id: u64) -> anyhow::Result<()> {
        let init = K0Packet {
            conn_id, packet_num: 0, flags: 0,
            frame_type: FrameType::HandshakeInit,
            payload: b"K0_INIT".to_vec(),
        };
        self.socket.send_to(&init.encode(), peer).await?;
        println!("[K0] INIT → {peer}");

        let mut buf = vec![0u8; 1500];
        tokio::select! {
            result = self.socket.recv_from(&mut buf) => {
                let (len, _) = result?;
                let reply = K0Packet::decode(&buf[..len])
                    .ok_or_else(|| anyhow::anyhow!("bad packet in handshake"))?;
                if reply.frame_type != FrameType::HandshakeInitAck {
                    return Err(anyhow::anyhow!("expected INIT_ACK, got {:?}", reply.frame_type));
                }
                println!("[K0] INIT_ACK ← {peer}");
            }
            _ = tokio::time::sleep(Duration::from_secs(3)) => {
                return Err(anyhow::anyhow!("handshake timeout"));
            }
        }

        let done = K0Packet {
            conn_id, packet_num: 1, flags: 0,
            frame_type: FrameType::HandshakeDone,
            payload: b"K0_DONE".to_vec(),
        };
        self.socket.send_to(&done.encode(), peer).await?;
        println!("[K0] ✓ Handshake complete");

        // register connection
        let mut map = self.connections.lock().await;
        let mut conn = K0Connection::new(conn_id, peer);
        conn.handshake_state = HandshakeState::Done;
        let s1 = conn.open_stream(true,  true);
        let s2 = conn.open_stream(false, false);
        println!("[K0] Local streams: reliable={s1} partial={s2}");
        map.insert(conn_id, ConnectionCtx::new(conn));
        drop(map);

        // spawn background recv loop so client processes ACKs while sending
        let conns  = Arc::clone(&self.connections);
        let socket = Arc::clone(&self.socket);
        Self::spawn_ticker(Arc::clone(&conns), Arc::clone(&socket));
        tokio::spawn(async move {
            let t = K0Transport { socket, connections: conns };
            let mut buf = vec![0u8; 1500];
            loop {
                match t.socket.recv_from(&mut buf).await {
                    Ok((len, peer)) => {
                        if let Some(pkt) = K0Packet::decode(&buf[..len]) {
                            t.handle_packet(pkt, peer).await;
                        }
                    }
                    Err(e) => { eprintln!("[K0] client recv: {e}"); break; }
                }
            }
        });

        Ok(())
    }

    pub async fn send_stream(&self, conn_id: u64, stream_id: u32, data: &[u8]) -> anyhow::Result<()> {
        let mut map = self.connections.lock().await;
        let ctx = map.get_mut(&conn_id).ok_or_else(|| anyhow::anyhow!("unknown conn_id"))?;
        if !ctx.conn.is_ready() { return Err(anyhow::anyhow!("not ready")); }

        let stream = ctx.conn.streams.get_mut(&stream_id)
            .ok_or_else(|| anyhow::anyhow!("unknown stream_id"))?;
        let ordered  = stream.ordered;
        let reliable = stream.reliable;
        let offset   = stream.take_offset(data.len());
        let pnum     = ctx.conn.next_packet_num();
        let peer     = ctx.conn.peer_addr;

        if !ctx.bbr.can_send(ctx.conn.in_flight_count()) {
            eprintln!("[K0] cwnd full ({}/{}) — sending anyway", ctx.conn.in_flight_count(), ctx.bbr.cwnd);
        }

        let pkt     = K0Packet::stream(conn_id, pnum, stream_id, offset, data, ordered, reliable);
        let encoded = pkt.encode();
        if reliable { ctx.conn.ack_tracker.track(pnum, encoded.clone()); }
        self.socket.send_to(&encoded, peer).await?;
        println!("[K0] → stream={stream_id} off={offset} {}B  rel={reliable}", data.len());

        if !reliable {
            let fec = ctx.fec_for(stream_id);
            if let Some(parity) = fec.feed(data) {
                let parity_pnum = ctx.conn.next_packet_num();
                let mut payload = stream_id.to_be_bytes().to_vec();
                payload.extend_from_slice(&parity);
                let fec_pkt = K0Packet {
                    conn_id, packet_num: parity_pnum,
                    flags: K0Packet::flags_for(false, false) | 0b0000_0100,
                    frame_type: FrameType::FecParity,
                    payload,
                };
                self.socket.send_to(&fec_pkt.encode(), peer).await?;
                println!("[K0] FEC parity → stream={stream_id} {}B", parity.len());
            }
        }

        Ok(())
    }

    pub async fn migrate_path(&self, conn_id: u64, new_peer: SocketAddr) -> anyhow::Result<()> {
        let mut map = self.connections.lock().await;
        let ctx = map.get_mut(&conn_id).ok_or_else(|| anyhow::anyhow!("unknown conn_id"))?;
        let nonce = rand_nonce();
        ctx.conn.pending_challenge = Some(nonce);
        let pnum = ctx.conn.next_packet_num();
        let challenge = K0Packet::path_challenge(conn_id, pnum, nonce);
        self.socket.send_to(&challenge.encode(), new_peer).await?;
        println!("[K0] PATH_CHALLENGE → {new_peer} nonce={nonce:#x}");
        Ok(())
    }

    pub async fn close(&self, conn_id: u64) -> anyhow::Result<()> {
        let mut map = self.connections.lock().await;
        if let Some(ctx) = map.remove(&conn_id) {
            let close = K0Packet {
                conn_id, packet_num: 0, flags: 0,
                frame_type: FrameType::Close, payload: vec![],
            };
            self.socket.send_to(&close.encode(), ctx.conn.peer_addr).await?;
            println!("[K0] Connection {conn_id:#x} closed");
        }
        Ok(())
    }

    pub async fn stats(&self, conn_id: u64) -> Option<ConnStats> {
        let map = self.connections.lock().await;
        let ctx = map.get(&conn_id)?;
        Some(ConnStats {
            srtt_ms:     ctx.conn.srtt_ms(),
            rto_ms:      ctx.conn.rto_ms(),
            cwnd:        ctx.bbr.cwnd,
            in_flight:   ctx.conn.in_flight_count(),
            min_rtt_ms:  ctx.bbr.min_rtt_ms(),
            max_bw_kbps: ctx.bbr.max_bw_kbps(),
            phase:       format!("{:?}", ctx.bbr.phase),
        })
    }
}

#[derive(Debug)]
pub struct ConnStats {
    pub srtt_ms:     u64,
    pub rto_ms:      u64,
    pub cwnd:        usize,
    pub in_flight:   usize,
    pub min_rtt_ms:  u64,
    pub max_bw_kbps: f64,
    pub phase:       String,
}

fn rand_nonce() -> u64 {
    use std::time::SystemTime;
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default().subsec_nanos() as u64;
    let mut x = t ^ 0x9e3779b97f4a7c15;
    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
    x
}