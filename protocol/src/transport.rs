#![allow(dead_code)]
// src/transport.rs — K.0 async transport (Phase 2 integrated)
// Wires: TLS handshake, per-packet encryption, flow control, 0-RTT session resumption

use crate::congestion::BbrController;
use crate::connection::{HandshakeState, K0Connection};
use crate::fec::FecEncoder;
use crate::flow::{ConnectionFlowControl, StreamFlowControl, WindowUpdate};
use crate::frame::{FrameDecoder, FrameKind, K0Frame};
use crate::hpack::HpackDecoder;
use crate::packet::{FrameType, K0Packet};
use crate::session::{SessionStore, SessionTicket, TicketIssuer};
use crate::tls::{PacketCrypto, TlsState};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time;

const TICK_INTERVAL_MS: u64 = 10;

// ─────────────────────────────────────────────────────────────────────────────

pub struct K0Transport {
    socket:      Arc<UdpSocket>,
    connections: Arc<Mutex<HashMap<u64, ConnectionCtx>>>,
    // server-side ticket issuer (None on pure clients)
    ticket_issuer: Arc<Mutex<Option<TicketIssuer>>>,
    // client-side session cache
    session_store: Arc<Mutex<SessionStore>>,
}

// ── Per-connection runtime context ────────────────────────────────────────────
struct ConnectionCtx {
    conn:      K0Connection,
    bbr:       BbrController,
    fec_enc:   HashMap<u32, FecEncoder>,
    fec_slot:  HashMap<u32, usize>,

    // Phase 2: crypto
    tls_state: TlsState,
    crypto:    Option<PacketCrypto>,  // Some after handshake, None during

    // Phase 2: flow control
    conn_flow: ConnectionFlowControl,
    stream_flow: HashMap<u32, StreamFlowControl>,

    // Phase 2: frame decoders per stream
    frame_decoders: HashMap<u32, FrameDecoder>,

    // Phase 2: pending 0-RTT data (held until handshake confirms)
    zero_rtt_buf: Vec<(u32, Vec<u8>)>, // (stream_id, data)
    is_resumed:   bool,
}

impl ConnectionCtx {
    fn new(conn: K0Connection) -> Self {
        Self {
            conn,
            bbr:            BbrController::new(),
            fec_enc:        HashMap::new(),
            fec_slot:       HashMap::new(),
            tls_state:      TlsState::None,
            crypto:         None,
            conn_flow:      ConnectionFlowControl::new(),
            stream_flow:    HashMap::new(),
            frame_decoders: HashMap::new(),
            zero_rtt_buf:   Vec::new(),
            is_resumed:     false,
        }
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

    fn flow_for(&mut self, stream_id: u32) -> &mut StreamFlowControl {
        self.stream_flow
            .entry(stream_id)
            .or_insert_with(|| StreamFlowControl::new(stream_id))
    }

    fn decoder_for(&mut self, stream_id: u32) -> &mut FrameDecoder {
        self.frame_decoders
            .entry(stream_id)
            .or_insert_with(FrameDecoder::new)
    }

    // Encrypt a packet payload if crypto is established, else pass through plaintext.
    fn maybe_encrypt(
        &self,
        packet_num: u64,
        flags:      u8,
        frame_type: u8,
        payload:    &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        match &self.crypto {
            Some(c) => {
                // AAD = conn_id bytes + flags + frame_type (authenticated but not encrypted)
                let aad = self.make_aad(flags, frame_type);
                c.encrypt(packet_num, &aad, payload)
            }
            None => Ok(payload.to_vec()),
        }
    }

    fn maybe_decrypt(
        &self,
        packet_num: u64,
        flags:      u8,
        frame_type: u8,
        payload:    &mut Vec<u8>,
    ) -> anyhow::Result<Vec<u8>> {
        match &self.crypto {
            Some(c) => {
                let aad = self.make_aad(flags, frame_type);
                let plain = c.decrypt(packet_num, &aad, payload)?;
                Ok(plain.to_vec())
            }
            None => Ok(payload.clone()),
        }
    }

    fn make_aad(&self, flags: u8, frame_type: u8) -> Vec<u8> {
        let mut aad = Vec::with_capacity(10);
        aad.extend_from_slice(&self.conn.conn_id.to_be_bytes());
        aad.push(flags);
        aad.push(frame_type);
        aad
    }
}

// ─────────────────────────────────────────────────────────────────────────────

impl K0Transport {
    pub async fn bind(addr: &str) -> anyhow::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        println!("[K0] Bound on {addr}");
        Ok(Self {
            socket,
            connections:   Arc::new(Mutex::new(HashMap::new())),
            ticket_issuer: Arc::new(Mutex::new(None)),
            session_store: Arc::new(Mutex::new(SessionStore::new())),
        })
    }

    /// Enable server-side session ticket issuance (call on servers).
    pub async fn enable_session_tickets(&self) -> anyhow::Result<()> {
        let issuer = TicketIssuer::new()?;
        *self.ticket_issuer.lock().await = Some(issuer);
        Ok(())
    }

    // ── Server loop ───────────────────────────────────────────────────────────
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

    // ── Inbound packet dispatch ───────────────────────────────────────────────
    async fn handle_packet(&self, mut pkt: K0Packet, peer: SocketAddr) {
        let mut map = self.connections.lock().await;

        match pkt.frame_type {

            // ── HANDSHAKE INIT ────────────────────────────────────────────────
            FrameType::HandshakeInit => {
                println!("[K0] INIT from {peer}  conn={:#x}", pkt.conn_id);

                // Check for 0-RTT session ticket in payload
                // Payload layout: b"K0_INIT" | optional [1B: 0x01][ticket bytes]
                let (_init_magic, ticket_data) = split_init_payload(&pkt.payload);
                let mut resumed = false;

                if let Some(ticket_bytes) = ticket_data {
                    if let Some(ticket) = SessionTicket::decode(ticket_bytes) {
                        let issuer_lock = self.ticket_issuer.lock().await;
                        if let Some(issuer) = issuer_lock.as_ref() {
                            if let Some(session) = issuer.validate(&ticket) {
                                println!(
                                    "[K0] 0-RTT resumed for {} age={}s",
                                    session.server_id, session.age_secs
                                );
                                resumed = true;
                                let ctx = map
                                    .entry(pkt.conn_id)
                                    .or_insert_with(|| ConnectionCtx::new(
                                        K0Connection::new(pkt.conn_id, peer)
                                    ));
                                ctx.conn.handshake_state = HandshakeState::Done;
                                ctx.is_resumed = true;
                                // Derive crypto from session keys
                                if session.keys.len() >= 32 {
                                    let mut key = [0u8; 32];
                                    key.copy_from_slice(&session.keys[..32]);
                                    if let Ok(crypto) = PacketCrypto::new(&key) {
                                        ctx.crypto = Some(crypto);
                                        ctx.tls_state = TlsState::Established;
                                    }
                                }
                                let s1 = ctx.conn.open_stream(true,  true);
                                let s2 = ctx.conn.open_stream(false, false);
                                println!("[K0] 0-RTT streams: reliable={s1} partial={s2}");
                            }
                        }
                    }
                }

                if !resumed {
                    // Normal handshake path
                    let ctx = map
                        .entry(pkt.conn_id)
                        .or_insert_with(|| ConnectionCtx::new(K0Connection::new(pkt.conn_id, peer)));
                    ctx.conn.handshake_state = HandshakeState::InitSent;
                    ctx.tls_state = TlsState::Handshaking;
                }

                let ctx = map.get_mut(&pkt.conn_id).unwrap();
                let pnum = ctx.conn.next_packet_num();
                let ack = K0Packet {
                    conn_id:    pkt.conn_id,
                    packet_num: pnum,
                    flags:      0,
                    frame_type: FrameType::HandshakeInitAck,
                    payload:    if resumed { b"K0_INIT_ACK_0RTT".to_vec() }
                                else      { b"K0_INIT_ACK".to_vec() },
                };
                let _ = self.socket.send_to(&ack.encode(), peer).await;
                println!("[K0] INIT_ACK → {peer} (0rtt={resumed})");
            }

            // ── HANDSHAKE DONE ────────────────────────────────────────────────
            FrameType::HandshakeDone => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    ctx.conn.handshake_state = HandshakeState::Done;

                    // Derive session crypto key from the DONE payload
                    // In production: use TLS exporter. Here: use first 32B of payload as key.
                    // Payload layout: b"K0_DONE" | [32B key material]
                    if pkt.payload.len() >= 39 {
                        let mut key = [0u8; 32];
                        key.copy_from_slice(&pkt.payload[7..39]);
                        if let Ok(crypto) = PacketCrypto::new(&key) {
                            ctx.crypto    = Some(crypto);
                            ctx.tls_state = TlsState::Established;
                            println!("[K0] Crypto established ✓");
                        }
                    }

                    let s1 = ctx.conn.open_stream(true,  true);
                    let s2 = ctx.conn.open_stream(false, false);
                    println!("[K0] ✓ Handshake done {peer} | reliable={s1} partial={s2}");

                    // Issue session ticket for future 0-RTT
                    let mut issuer_lock = self.ticket_issuer.lock().await;
                    if let Some(issuer) = issuer_lock.as_mut() {
                        let keys = pkt.payload[7..].to_vec();
                        if let Ok(ticket) = issuer.issue("k0-server", "k0/1.0", keys) {
                            let ticket_bytes = ticket.encode();
                            let pnum2 = ctx.conn.next_packet_num();
                            let ticket_pkt = K0Packet {
                                conn_id:    pkt.conn_id,
                                packet_num: pnum2,
                                flags:      0,
                                frame_type: FrameType::Control,
                                // Control sub-type 0x01 = SESSION_TICKET
                                payload:    [&[0x01u8], ticket_bytes.as_slice()].concat(),
                            };
                            let _ = self.socket.send_to(&ticket_pkt.encode(), peer).await;
                            println!("[K0] Session ticket issued → {peer}");
                        }
                    }

                    // Flush any buffered 0-RTT data that arrived before handshake
                    let buffered: Vec<_> = ctx.zero_rtt_buf.drain(..).collect();
                    for (stream_id, data) in buffered {
                        if let Some(stream) = ctx.conn.streams.get_mut(&stream_id) {
                            stream.recv_buf.push(data);
                        }
                    }
                }
            }

            // ── STREAM DATA ───────────────────────────────────────────────────
            FrameType::Stream => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    if !ctx.conn.is_ready() && !ctx.is_resumed {
                        eprintln!("[K0] Stream before handshake — dropped");
                        return;
                    }

                    // Decrypt payload if crypto is established
                    let decrypted = match ctx.maybe_decrypt(
                        pkt.packet_num,
                        pkt.flags,
                        pkt.frame_type.to_byte(),
                        &mut pkt.payload,
                    ) {
                        Ok(d) => d,
                        Err(e) => { eprintln!("[K0] Decrypt failed: {e}"); return; }
                    };

                    if let Some((stream_id, offset, raw_data)) =
                        K0Packet::parse_stream_header(&decrypted)
                    {
                        let reliable = pkt.is_reliable();
                        let ordered  = pkt.is_ordered();

                        // Flow control: check receiver window
                        let flow = ctx.flow_for(stream_id);
                        if let Err(e) = flow.recv.on_receive(raw_data.len()) {
                            eprintln!("[K0] Flow control violation: {e}");
                            return;
                        }

                        // Feed FEC decoder slot on unreliable streams
                        if !reliable {
                            let slot = ctx.next_fec_slot(stream_id);
                            if let Some(stream) = ctx.conn.streams.get_mut(&stream_id) {
                                if let Some(fec) = &mut stream.fec {
                                    fec.receive_data(slot, raw_data.to_vec());
                                }
                            }
                        }

                        // Feed frame decoder (HTTP/K.0 framing layer)
                        let decoder = ctx.decoder_for(stream_id);
                        decoder.feed(raw_data);
                        let frames = decoder.drain_frames();
                        for frame in &frames {
                            match frame.kind {
                                FrameKind::Headers => {
                                    let dec = HpackDecoder::new();
                                    if let Ok(headers) = dec.decode(&frame.payload) {
                                        let method = headers.iter()
                                            .find(|h| h.name == ":method")
                                            .map(|h| h.value.as_str())
                                            .unwrap_or("?");
                                        let path = headers.iter()
                                            .find(|h| h.name == ":path")
                                            .map(|h| h.value.as_str())
                                            .unwrap_or("?");
                                        println!(
                                            "[K0] HTTP {method} {path} on stream={stream_id}"
                                        );
                                    }
                                }
                                FrameKind::Data => {
                                    println!(
                                        "[K0] DATA {}B on stream={stream_id}",
                                        frame.payload.len()
                                    );
                                }
                                _ => {}
                            }
                        }

                        if let Some(stream) = ctx.conn.streams.get_mut(&stream_id) {
                            println!(
                                "[K0] stream={stream_id} off={offset} {}B  \
                                 ordered={ordered} reliable={reliable} \
                                 encrypted={}",
                                raw_data.len(),
                                ctx.crypto.is_some()
                            );
                            stream.receive(offset, raw_data.to_vec());
                            if reliable {
                                ctx.conn.ack_tracker.queue_ack(pkt.packet_num);
                            }
                        }

                        // Check if we should send a WINDOW_UPDATE
                        let flow = ctx.flow_for(stream_id);
                        flow.recv.on_consume(raw_data.len()); // treat receive = consume for now
                        if let Some(increment) = flow.recv.should_update() {
                            flow.recv.mark_updated(increment);
                            let wu = WindowUpdate { stream_id, increment: increment as u32 };
                            let pnum = ctx.conn.next_packet_num();
                            let wu_pkt = K0Packet {
                                conn_id:    pkt.conn_id,
                                packet_num: pnum,
                                flags:      0,
                                frame_type: FrameType::Control,
                                // Control sub-type 0x02 = WINDOW_UPDATE
                                payload: [&[0x02u8], wu.encode().as_slice()].concat(),
                            };
                            let _ = self.socket.send_to(&wu_pkt.encode(), peer).await;
                        }

                        // Flush ACKs
                        let pending = ctx.conn.ack_tracker.drain_acks();
                        for ack_pkt_num in pending {
                            let pnum = ctx.conn.next_packet_num();
                            let ack  = K0Packet::ack(pkt.conn_id, pnum, ack_pkt_num, 0);
                            let _ = self.socket.send_to(&ack.encode(), peer).await;
                        }
                    }
                }
            }

            // ── ACK ───────────────────────────────────────────────────────────
            FrameType::Ack => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    if let Some((acked, delay_us)) = K0Packet::parse_ack(&pkt.payload) {
                        ctx.conn.ack_tracker.on_ack(acked, delay_us);
                        let rtt = ctx.conn.ack_tracker.smoothed_rtt;
                        ctx.bbr.on_ack(1350, rtt);
                        println!(
                            "[K0] ACK pkt={acked}  srtt={}ms  cwnd={}",
                            ctx.conn.srtt_ms(), ctx.bbr.cwnd
                        );
                    }
                }
            }

            // ── NAK ───────────────────────────────────────────────────────────
            FrameType::Nak => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    if pkt.payload.len() >= 8 {
                        let lost = u64::from_be_bytes(pkt.payload[0..8].try_into().unwrap());
                        ctx.conn.ack_tracker.on_nak(lost);
                        ctx.bbr.on_loss();
                        println!("[K0] NAK lost={lost}");
                    }
                }
            }

            // ── FEC PARITY ────────────────────────────────────────────────────
            FrameType::FecParity => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    if pkt.payload.len() >= 4 {
                        let stream_id = u32::from_be_bytes(pkt.payload[0..4].try_into().unwrap());
                        let parity    = pkt.payload[4..].to_vec();
                        ctx.reset_fec_slot(stream_id);
                        if let Some(stream) = ctx.conn.streams.get_mut(&stream_id) {
                            if let Some(fec) = &mut stream.fec {
                                fec.receive_parity(parity);
                                match fec.recover() {
                                    crate::fec::FecResult::Recovered { slot, data } => {
                                        println!("[K0] FEC ✓ stream={stream_id} slot={slot}");
                                        stream.recv_buf.push(data);
                                        fec.reset();
                                    }
                                    crate::fec::FecResult::Complete => {
                                        println!("[K0] FEC complete stream={stream_id}");
                                        fec.reset();
                                    }
                                    crate::fec::FecResult::Unrecoverable => {
                                        eprintln!("[K0] FEC unrecoverable stream={stream_id}");
                                        fec.reset();
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── CONTROL (WINDOW_UPDATE / SESSION_TICKET) ──────────────────────
            FrameType::Control => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    if pkt.payload.is_empty() { return; }
                    match pkt.payload[0] {
                        // 0x01 = SESSION_TICKET (client receives from server)
                        0x01 => {
                            if let Some(ticket) = SessionTicket::decode(&pkt.payload[1..]) {
                                println!(
                                    "[K0] Session ticket received for '{}' — caching for 0-RTT",
                                    ticket.server_id
                                );
                                // store in session store (client-side)
                                // We can't easily reach self.session_store here from handle_packet
                                // because we hold the connections lock. Store ticket in conn ctx
                                // and drain it after the lock is released.
                                // For now: log it. Full wiring in connect() post-handshake.
                            }
                        }
                        // 0x02 = WINDOW_UPDATE (received from peer)
                        0x02 => {
                            if let Some(wu) = WindowUpdate::decode(&pkt.payload[1..]) {
                                if wu.stream_id == 0 {
                                    // connection-level update
                                    if let Err(e) = ctx.conn_flow.send.add_credit(wu.increment as u64) {
                                        eprintln!("[K0] Window update error: {e}");
                                    } else {
                                        println!("[K0] Conn window +{}", wu.increment);
                                    }
                                } else {
                                    let flow = ctx.flow_for(wu.stream_id);
                                    if let Err(e) = flow.send.add_credit(wu.increment as u64) {
                                        eprintln!("[K0] Stream window error: {e}");
                                    } else {
                                        println!(
                                            "[K0] Stream={} window +{}",
                                            wu.stream_id, wu.increment
                                        );
                                    }
                                }
                            }
                        }
                        sub => println!("[K0] Unknown control sub-type {sub:#x}"),
                    }
                }
            }

            // ── PATH CHALLENGE ────────────────────────────────────────────────
            FrameType::PathChallenge => {
                if pkt.payload.len() >= 8 {
                    let nonce = u64::from_be_bytes(pkt.payload[0..8].try_into().unwrap());
                    if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                        let pnum = ctx.conn.next_packet_num();
                        let resp = K0Packet::path_response(pkt.conn_id, pnum, nonce);
                        let _ = self.socket.send_to(&resp.encode(), peer).await;
                        println!("[K0] PATH_RESPONSE → {peer}");
                    }
                }
            }

            // ── PATH RESPONSE ─────────────────────────────────────────────────
            FrameType::PathResponse => {
                if let Some(ctx) = map.get_mut(&pkt.conn_id) {
                    if pkt.payload.len() >= 8 {
                        let nonce = u64::from_be_bytes(pkt.payload[0..8].try_into().unwrap());
                        if ctx.conn.pending_challenge == Some(nonce) {
                            ctx.conn.pending_challenge = None;
                            ctx.conn.peer_addr = peer;
                            println!("[K0] ✓ Path migrated to {peer}");
                        }
                    }
                }
            }

            // ── CLOSE ─────────────────────────────────────────────────────────
            FrameType::Close => {
                map.remove(&pkt.conn_id);
                println!("[K0] Connection {:#x} closed by {peer}", pkt.conn_id);
            }

            _ => println!("[K0] Unhandled {:?} from {peer}", pkt.frame_type),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // CLIENT API
    // ─────────────────────────────────────────────────────────────────────────

    pub async fn connect(&self, peer: SocketAddr, conn_id: u64) -> anyhow::Result<()> {
        // Generate session key material (32 random bytes)
        // In production: derive from TLS exporter after rustls handshake.
        let key = PacketCrypto::random_key()?;

        // Check for existing session ticket → 0-RTT fast path
        let ticket_bytes = {
            let store = self.session_store.lock().await;
            store.get("k0-server").map(|t| t.encode())
        };

        // Build INIT payload: b"K0_INIT" | optional [0x01][ticket]
        let init_payload = match &ticket_bytes {
            Some(tb) => {
                let mut p = b"K0_INIT".to_vec();
                p.push(0x01);
                p.extend_from_slice(tb);
                println!("[K0] Attempting 0-RTT with cached session ticket");
                p
            }
            None => b"K0_INIT".to_vec(),
        };

        let init = K0Packet {
            conn_id, packet_num: 0, flags: 0,
            frame_type: FrameType::HandshakeInit,
            payload: init_payload,
        };
        self.socket.send_to(&init.encode(), peer).await?;
        println!("[K0] INIT → {peer}");

        // Wait for INIT_ACK
        let mut buf = vec![0u8; 1500];
        let zero_rtt = tokio::select! {
            result = self.socket.recv_from(&mut buf) => {
                let (len, _) = result?;
                let reply = K0Packet::decode(&buf[..len])
                    .ok_or_else(|| anyhow::anyhow!("bad packet in handshake"))?;
                if reply.frame_type != FrameType::HandshakeInitAck {
                    return Err(anyhow::anyhow!("expected INIT_ACK"));
                }
                let resumed = reply.payload == b"K0_INIT_ACK_0RTT";
                println!("[K0] INIT_ACK ← {peer} (0rtt={resumed})");
                resumed
            }
            _ = tokio::time::sleep(Duration::from_secs(3)) => {
                return Err(anyhow::anyhow!("handshake timeout"));
            }
        };

        // Send DONE with key material embedded
        // Payload: b"K0_DONE" | [32B key]
        let mut done_payload = b"K0_DONE".to_vec();
        done_payload.extend_from_slice(&key);
        let done = K0Packet {
            conn_id, packet_num: 1, flags: 0,
            frame_type: FrameType::HandshakeDone,
            payload: done_payload,
        };
        self.socket.send_to(&done.encode(), peer).await?;
        println!("[K0] ✓ Handshake complete (key material sent)");

        // Register connection locally with crypto
        let mut map = self.connections.lock().await;
        let mut conn = K0Connection::new(conn_id, peer);
        conn.handshake_state = HandshakeState::Done;
        let s1 = conn.open_stream(true,  true);
        let s2 = conn.open_stream(false, false);
        println!("[K0] Local streams: reliable={s1} partial={s2}");

        let mut ctx = ConnectionCtx::new(conn);
        ctx.crypto    = Some(PacketCrypto::new(&key)?);
        ctx.tls_state = TlsState::Established;
        ctx.is_resumed = zero_rtt;
        map.insert(conn_id, ctx);
        drop(map);

        // Spawn background recv loop (processes ACKs, window updates, tickets)
        let conns      = Arc::clone(&self.connections);
        let socket     = Arc::clone(&self.socket);
        let sess_store = Arc::clone(&self.session_store);
        let issuer     = Arc::clone(&self.ticket_issuer);
        Self::spawn_ticker(Arc::clone(&conns), Arc::clone(&socket));

        tokio::spawn(async move {
            let t = K0Transport {
                socket,
                connections:   conns,
                ticket_issuer: issuer,
                session_store: sess_store,
            };
            let mut buf = vec![0u8; 1500];
            loop {
                match t.socket.recv_from(&mut buf).await {
                    Ok((len, peer)) => {
                        if let Some(pkt) = K0Packet::decode(&buf[..len]) {
                            // If it's a session ticket control frame, cache it
                            if pkt.frame_type == FrameType::Control
                                && pkt.payload.first() == Some(&0x01)
                            {
                                if let Some(ticket) = SessionTicket::decode(&pkt.payload[1..]) {
                                    let mut store = t.session_store.lock().await;
                                    store.store(ticket.server_id.clone(), ticket);
                                    println!("[K0] Session ticket cached for future 0-RTT");
                                }
                            } else {
                                t.handle_packet(pkt, peer).await;
                            }
                        }
                    }
                    Err(e) => { eprintln!("[K0] client recv: {e}"); break; }
                }
            }
        });

        Ok(())
    }

    // ── Send stream data (encrypted + flow controlled + framed) ──────────────
    pub async fn send_stream(
        &self,
        conn_id:   u64,
        stream_id: u32,
        data:      &[u8],
    ) -> anyhow::Result<()> {
        let mut map = self.connections.lock().await;
        let ctx = map.get_mut(&conn_id)
            .ok_or_else(|| anyhow::anyhow!("unknown conn_id"))?;

        if !ctx.conn.is_ready() {
            return Err(anyhow::anyhow!("connection not ready"));
        }

        let stream = ctx.conn.streams.get_mut(&stream_id)
            .ok_or_else(|| anyhow::anyhow!("unknown stream_id"))?;

        let ordered  = stream.ordered;
        let reliable = stream.reliable;
        let offset   = stream.take_offset(data.len());
        let peer     = ctx.conn.peer_addr;

        // Flow control gate: check both stream + connection window
        {
            let flow = ctx.flow_for(stream_id);
            if !flow.send.can_send(data.len()) {
                eprintln!(
                    "[K0] Stream={stream_id} flow blocked ({} bytes available)",
                    flow.send.available
                );
                // In production: enqueue. Here: send anyway (window refills via updates).
            }
            flow.send.consume(data.len());
            ctx.conn_flow.send.consume(data.len());
        }

        // BBR gate
        if !ctx.bbr.can_send(ctx.conn.in_flight_count()) {
            eprintln!("[K0] cwnd full — sending anyway");
        }

        let pnum  = ctx.conn.next_packet_num();
        let flags = K0Packet::flags_for(ordered, reliable);

        // Encrypt the stream payload
        let raw_stream_payload = {
            let mut p = Vec::with_capacity(8 + data.len());
            p.extend_from_slice(&stream_id.to_be_bytes());
            p.extend_from_slice(&offset.to_be_bytes());
            p.extend_from_slice(data);
            p
        };

        let encrypted_payload = ctx.maybe_encrypt(
            pnum,
            flags,
            FrameType::Stream.to_byte(),
            &raw_stream_payload,
        )?;

        let pkt = K0Packet {
            conn_id,
            packet_num: pnum,
            flags,
            frame_type: FrameType::Stream,
            payload: encrypted_payload.clone(),
        };
        let encoded = pkt.encode();

        if reliable {
            ctx.conn.ack_tracker.track(pnum, encoded.clone());
        }

        self.socket.send_to(&encoded, peer).await?;
        println!(
            "[K0] → stream={stream_id} off={offset} {}B  rel={reliable}  enc={}",
            data.len(), ctx.crypto.is_some()
        );

        // FEC on unreliable streams (feed raw data, not encrypted — receiver decrypts first)
        if !reliable {
            let fec = ctx.fec_for(stream_id);
            if let Some(parity) = fec.feed(data) {
                let parity_pnum = ctx.conn.next_packet_num();
                let mut payload = stream_id.to_be_bytes().to_vec();
                payload.extend_from_slice(&parity);
                let fec_pkt = K0Packet {
                    conn_id,
                    packet_num: parity_pnum,
                    flags: K0Packet::flags_for(false, false) | 0b0000_0100,
                    frame_type: FrameType::FecParity,
                    payload,
                };
                self.socket.send_to(&fec_pkt.encode(), peer).await?;
                println!("[K0] FEC parity → stream={stream_id}");
            }
        }

        Ok(())
    }

    // ── Send an HTTP/K.0 request (HEADERS + DATA frames, compressed) ──────────
    pub async fn send_request(
        &self,
        conn_id:   u64,
        stream_id: u32,
        req:       &crate::hpack::K0Request,
    ) -> anyhow::Result<()> {
        // Encode HEADERS frame
        let headers_frame = K0Frame::headers(req.encode_headers().into());
        let mut wire = headers_frame.encode().to_vec();

        // If body exists, append DATA frame
        if !req.body.is_empty() {
            wire.extend_from_slice(&K0Frame::data(req.body.clone()).encode());
        }
        

        self.send_stream(conn_id, stream_id, &wire).await
    }

    // ── Send an HTTP/K.0 response ─────────────────────────────────────────────
    pub async fn send_response(
        &self,
        conn_id:   u64,
        stream_id: u32,
        resp:      &crate::hpack::K0Response,
    ) -> anyhow::Result<()> {
        let headers_frame = K0Frame::headers(resp.encode_headers().into());
        let mut wire = headers_frame.encode().to_vec();
        if !resp.body.is_empty() {
            wire.extend_from_slice(&K0Frame::data(resp.body.clone()).encode());
        }
        self.send_stream(conn_id, stream_id, &wire).await
    }

    // ── Path migration ────────────────────────────────────────────────────────
    pub async fn migrate_path(&self, conn_id: u64, new_peer: SocketAddr) -> anyhow::Result<()> {
        let mut map = self.connections.lock().await;
        let ctx = map.get_mut(&conn_id)
            .ok_or_else(|| anyhow::anyhow!("unknown conn_id"))?;
        let nonce = rand_nonce();
        ctx.conn.pending_challenge = Some(nonce);
        let pnum = ctx.conn.next_packet_num();
        let challenge = K0Packet::path_challenge(conn_id, pnum, nonce);
        self.socket.send_to(&challenge.encode(), new_peer).await?;
        println!("[K0] PATH_CHALLENGE → {new_peer}");
        Ok(())
    }

    // ── Graceful close ────────────────────────────────────────────────────────
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

    // ── Stats ─────────────────────────────────────────────────────────────────
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
            encrypted:   ctx.tls_state == TlsState::Established,
            zero_rtt:    ctx.is_resumed,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ConnStats {
    pub srtt_ms:     u64,
    pub rto_ms:      u64,
    pub cwnd:        usize,
    pub in_flight:   usize,
    pub min_rtt_ms:  u64,
    pub max_bw_kbps: f64,
    pub phase:       String,
    pub encrypted:   bool,
    pub zero_rtt:    bool,
}

// ── Parse INIT payload for optional session ticket ────────────────────────────
// Format: b"K0_INIT" | optional [0x01][ticket bytes]
fn split_init_payload(payload: &[u8]) -> (&[u8], Option<&[u8]>) {
    if payload.len() > 8 && payload[7] == 0x01 {
        (&payload[..7], Some(&payload[8..]))
    } else {
        (payload, None)
    }
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