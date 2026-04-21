# HTTP/K.0 Browser

> A next-generation, low-latency browser and custom transport protocol written in Rust — optimized for cloud gaming, real-time streaming, and high-concurrency web applications.

---

## Table of Contents

- [Overview](#overview)
- [Protocol Design](#protocol-design)
- [Architecture](#architecture)
- [Modules](#modules)
  - [Transport Layer](#transport-layer)
  - [Connection State Machine](#connection-state-machine)
  - [Packet Codec](#packet-codec)
  - [HTTP/K.0 Framing](#httpk0-framing)
  - [ACK & Retransmission](#ack--retransmission)
  - [BBR-Lite Congestion Control](#bbr-lite-congestion-control)
  - [FEC Recovery](#fec-recovery)
  - [0-RTT Session Resumption](#0-rtt-session-resumption)
  - [TLS 1.3 & Per-Packet Encryption](#tls-13--per-packet-encryption)
  - [HPACK Header Compression](#hpack-header-compression)
  - [Flow Control](#flow-control)
  - [Hybrid TCP/UDP Transport](#hybrid-tcpudp-transport)
- [Packet & Frame Wire Format](#packet--frame-wire-format)
- [Development Status](#development-status)
- [Getting Started](#getting-started)
- [Roadmap](#roadmap)
- [Technical Stack](#technical-stack)

---

## Overview

**HTTP/K.0** is a custom application-layer protocol built on top of UDP, taking inspiration from QUIC (RFC 9000) while optimizing specifically for browser-level gaming and streaming workloads. Unlike QUIC, which targets general-purpose HTTP/3 delivery, HTTP/K.0 makes opinionated choices: BBR-based congestion control for latency-over-throughput, XOR FEC for fast loss recovery without retransmit delays, per-packet AEAD encryption outside of TLS record framing, and 0-RTT session resumption to eliminate handshake round-trips on repeat connections.

The codebase is split into three layers:

| Layer | Purpose |
|---|---|
| `protocol/` | HTTP/K.0 protocol — transport, framing, crypto, congestion |
| `Network Stack/` | Socket programming foundations (chat, TCP/UDP primitives) |
| `rust_basics/` | Rust language learning and browser concept prototyping |

---

## Protocol Design

HTTP/K.0 departs from TCP-based HTTP stacks in several key ways:

**UDP as the transport substrate.** TCP's head-of-line blocking is the enemy of real-time apps. A single lost packet stalls all subsequent data. UDP allows independent streams — a dropped game-state update doesn't block the next one.

**Delay-based congestion control (BBR), not loss-based (CUBIC).** CUBIC halves its window on packet loss, causing throughput spikes. BBR estimates bandwidth and minimum RTT, pacing sends to match the bottleneck link without inducing queue buildup. This is critical for low-latency gaming.

**Partial reliability.** Streams are individually flagged as ordered/unordered and reliable/partial. Game state updates can use unordered + partial delivery (low latency). Chat and HTTP headers use ordered + reliable (correctness). FEC provides a middle path: recover from single-packet loss without a retransmit round-trip.

**0-RTT reconnects.** After the first connection, the server issues a signed session ticket. On reconnect, the client presents the ticket and the server accepts data in the same packet — zero additional round-trips.

**Per-packet encryption.** Each UDP packet payload is independently sealed with ChaCha20-Poly1305, using the packet number as the nonce. This is stateless and allows out-of-order decryption, unlike TLS record framing which requires sequential decryption.

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    Application Layer                     │
│           (Browser Engine — Phase 3, planned)            │
├─────────────────────────────────────────────────────────┤
│                  HTTP/K.0 Framing (frame.rs)             │
│     HEADERS · DATA · TRAILERS · RESET · PUSH · PING     │
├─────────────────────────────────────────────────────────┤
│               HTTP/K.0 Transport (transport.rs)          │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │  TLS 1.3 +   │  │  Flow        │  │  0-RTT        │  │
│  │  ChaCha20    │  │  Control     │  │  Sessions     │  │
│  │  (tls.rs)    │  │  (flow.rs)   │  │  (session.rs) │  │
│  └──────────────┘  └──────────────┘  └───────────────┘  │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │  HPACK       │  │  BBR-Lite    │  │  XOR FEC      │  │
│  │  Compression │  │  Congestion  │  │  Recovery     │  │
│  │  (hpack.rs)  │  │  (congestion)│  │  (fec.rs)     │  │
│  └──────────────┘  └──────────────┘  └───────────────┘  │
├─────────────────────────────────────────────────────────┤
│             Connection State Machine (connection.rs)     │
│          Stream Multiplexing · ACK Tracking (ack.rs)     │
├─────────────────────────────────────────────────────────┤
│               Packet Codec (packet.rs)                   │
│          22B Header · Frame Types · Flags · Codec        │
├─────────────────────────────────────────────────────────┤
│              UDP (tokio::net::UdpSocket)                  │
└─────────────────────────────────────────────────────────┘
```

---

## Modules

### Transport Layer

**`protocol/src/transport.rs`** — `K0Transport`

The top-level async orchestrator. Binds to a UDP socket and manages all active connections, dispatching incoming packets to the appropriate connection context and applying the full protocol stack: decryption → flow control check → frame assembly → ACK generation → delivery.

**Key type: `ConnectionCtx`**

```
ConnectionCtx {
    conn:         K0Connection     // state machine + streams
    bbr:          BbrController    // congestion window
    crypto:       PacketCrypto     // ChaCha20-Poly1305 keys
    conn_flow:    FlowWindows      // connection-level flow control
    stream_flow:  FlowWindows      // per-stream flow control
    frame_decoders                 // stateful HTTP frame assembly per stream
    zero_rtt_buf                   // buffered 0-RTT frames pending handshake confirm
}
```

**Handshake state machine:**

```
Idle → InitSent → InitAckReceived → Done
         │                               │
         └── 0-RTT path (ticket) ────────┘
             (skips InitAck round-trip)
```

**Core API:**

| Method | Description |
|---|---|
| `bind(addr)` | Start listening on a UDP address |
| `enable_session_tickets()` | Enable 0-RTT on the server side |
| `connect(peer, conn_id)` | Initiate a connection as client |
| `send_stream(conn_id, stream_id, data)` | Send data with encryption + flow control |
| `send_request(conn_id, stream_id, req)` | Send an HTTP/K.0 GET or POST |
| `send_response(conn_id, stream_id, resp)` | Send an HTTP/K.0 response |
| `migrate_path(new_peer)` | Trigger connection migration (NAT rebinding) |
| `stats(conn_id)` | Return `ConnStats` (RTT, cwnd, bytes sent/recv) |
| `run()` | Server event loop — blocks, handles all inbound packets |

---

### Connection State Machine

**`protocol/src/connection.rs`** — `K0Connection`

Manages the per-connection lifecycle and stream multiplexing table.

**Stream model:**

Each stream is independently configured at open time:

| Flag | Options |
|---|---|
| `ordered` | Ordered (sequence-guaranteed) or Unordered (immediate delivery) |
| `reliable` | Reliable (retransmit on loss) or Partial (FEC-only recovery) |

Ordered + reliable streams use a reorder buffer (`BTreeMap<offset, bytes>`) and only deliver to the application in-sequence. Unordered + partial streams deliver immediately on arrival, relying on FEC to reconstruct any single lost shard.

**Stream states:** `Open → HalfClosed → Closed`

**Path migration:** Stores a `pending_challenge: u64` while a `PATH_CHALLENGE` is in flight. Confirmed on matching `PATH_RESPONSE`.

---

### Packet Codec

**`protocol/src/packet.rs`**

Defines the on-wire layout for every UDP packet sent over HTTP/K.0.

#### Packet Header — 22 bytes

```
┌──────────────────┬──────────────────┬──────┬────────────┬─────────────┐
│  conn_id (8B)    │  packet_num (8B)  │flags │ frame_type │ payload_len │
│  u64 big-endian  │  u64 big-endian   │ (1B) │    (1B)    │  (4B u32)   │
└──────────────────┴──────────────────┴──────┴────────────┴─────────────┘
```

#### Packet Flags (3 bits used)

| Bit | Meaning |
|---|---|
| 0 | `ordered` — ordered (1) / unordered (0) stream |
| 1 | `reliable` — reliable (1) / partial (0) stream |
| 2 | `fec_parity` — this packet is a FEC parity shard |

#### Frame Types

| Value | Name | Purpose |
|---|---|---|
| `0x00` | `HandshakeInit` | Client → Server: initiate connection |
| `0x01` | `HandshakeInitAck` | Server → Client: acknowledge init |
| `0x02` | `HandshakeDone` | Client → Server: complete handshake |
| `0x10` | `Stream` | Data frames: `[4B stream_id][4B offset][data...]` |
| `0x20` | `Ack` | Acknowledge received packet numbers |
| `0x21` | `Nak` | Explicit loss notification |
| `0x30` | `PathChallenge` | Validate new network path |
| `0x31` | `PathResponse` | Confirm path challenge |
| `0x40` | `FecParity` | XOR parity shard for loss recovery |
| `0x50` | `Control` | `WINDOW_UPDATE`, `SESSION_TICKET`, etc. |
| `0xFF` | `Close` | Graceful connection teardown |

---

### HTTP/K.0 Framing

**`protocol/src/frame.rs`** — `K0Frame`, `FrameDecoder`

Layered above the transport stream, HTTP/K.0 frames define what bytes mean as HTTP. A `FrameDecoder` is maintained per stream to reassemble frames from potentially fragmented transport chunks.

#### Frame Wire Format

```
┌──────────┬─────────────┬─────────────────┐
│ kind (1B)│  length (4B)│    payload      │
└──────────┴─────────────┴─────────────────┘
```

#### Frame Types

| Value | Name | Payload |
|---|---|---|
| `0x01` | `HEADERS` | HPACK-compressed request/response headers |
| `0x02` | `DATA` | Body bytes |
| `0x03` | `TRAILERS` | Trailing headers (after body, before stream close) |
| `0x04` | `RESET` | Stream cancel with error code |
| `0x05` | `SETTINGS` | Connection configuration parameters |
| `0x06` | `PUSH` | Server push (push_id + headers) |
| `0x07` | `GOAWAY` | Graceful connection shutdown |
| `0x08` | `PING` | Keepalive / RTT probe |

---

### ACK & Retransmission

**`protocol/src/ack.rs`** — `AckTracker`

Implements reliable delivery for ordered streams via sliding-window ACKs and adaptive retransmission.

**RTT estimation — RFC 6298 EWMA:**

```
initial:   smoothed_rtt = 200ms,  rtt_var = 50ms
on sample: smoothed_rtt = 0.875 × smoothed_rtt + 0.125 × sample
           rtt_var      = 0.750 × rtt_var      + 0.250 × |smoothed_rtt - sample|
rto        = smoothed_rtt + 4 × rtt_var   (clamped: 50ms – 5000ms)
```

**In-flight tracking:** `BTreeMap<packet_num → InFlight { data, sent_at, retries }>`

- On ACK: compute sample RTT, update estimate, remove from map
- On NAK or timeout (`tick()` called ~10ms): move to retransmit queue, double RTO
- Retransmitted packets reset retry counter; after max retries the stream is marked as failed

---

### BBR-Lite Congestion Control

**`protocol/src/congestion.rs`** — `BbrController`

CUBIC halves its congestion window on any packet loss, causing throughput oscillations that spike latency. BBR (Bottleneck Bandwidth and RTT) instead estimates the bottleneck bandwidth and minimum path RTT, pacing sends to match the link without building queues.

**Phases:**

| Phase | Pacing Gain | Purpose |
|---|---|---|
| `Startup` | 2.0× | Exponential window growth until BW plateaus |
| `Drain` | 0.75× | Drain queue built up during Startup |
| `ProbeBw` | 1.25→1.0× cycle | Cruise at estimated BW, probe for more periodically |
| `ProbeRtt` | 0.5× | Reduce cwnd to 4 packets to re-measure min RTT |

**ProbeBw 8-slot pacing cycle:**
```
[1.25, 0.75, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]
  ↑ probe up   ↑ drain   ↑─── cruise ───────
```

**Key metrics:**

| Metric | Description |
|---|---|
| `cwnd` | Congestion window in packets |
| `max_bw` | Peak bandwidth sample (bytes/ms), 2-second window |
| `min_rtt` | Minimum observed RTT, expires after 10 seconds |
| `pacing_rate` | Target send rate: `pacing_gain × max_bw` |

`ProbeRtt` is triggered once every 10 seconds to re-establish the true minimum RTT, preventing `min_rtt` from inflating over time.

---

### FEC Recovery

**`protocol/src/fec.rs`** — `FecEncoder`, `FecDecoder`

For unreliable streams (game state, audio deltas), a retransmit round-trip costs more than the loss itself. XOR FEC provides single-packet loss recovery without any additional latency.

**Encoding:** Every 4 data packets (`DEFAULT_GROUP_SIZE`), the encoder XORs all payloads together and sends a 5th parity shard. The parity shard is flagged with `fec_parity = 1` in the packet flags.

**Recovery:**

| Scenario | Outcome |
|---|---|
| 0 losses | `Complete` — all 4 data packets received normally |
| 1 loss | `Recovered { slot, data }` — reconstruct from the 3 survivors + parity |
| 2+ losses | `Unrecoverable` — XOR cannot recover multiple erasures |

For streaming workloads, bursty single-packet loss is the common case. Two-packet bursts trigger graceful degradation (skip frame / interpolate) rather than stalling.

---

### 0-RTT Session Resumption

**`protocol/src/session.rs`** — `SessionStore`, `TicketIssuer`

After a full handshake, the server issues a signed `SessionTicket` blob. On reconnect, the client includes the ticket in the `HandshakeInit` packet and the server validates it — skipping the `HandshakeInitAck` round-trip entirely.

**First connection (full handshake — 1 RTT):**
```
Client                          Server
  │─── HandshakeInit ──────────────▶│
  │◀── HandshakeInitAck ────────────│
  │─── HandshakeDone + key ─────────▶│
  │◀── SessionTicket (in Control) ──│
  │─── HEADERS (first request) ─────▶│
```

**Repeat connection (0-RTT fast path):**
```
Client                          Server
  │─── HandshakeInit + Ticket ──────▶│  ← ticket validated immediately
  │─── HEADERS + DATA (0-RTT) ──────▶│  ← data in same or next packet
  │◀── HandshakeInitAck_0RTT ────────│
```

**`SessionTicket` structure:**

| Field | Type | Description |
|---|---|---|
| `data` | `Vec<u8>` | Opaque encrypted ticket blob |
| `issued_at` | `u64` | Unix timestamp of issuance |
| `server_id` | `String` | Server identity (for cache keying) |
| `protocol` | `String` | ALPN negotiated (`"k0/1.0"`) |

**Lifetime:** 24 hours (`SESSION_LIFETIME_SECS`).

**Security note:** 0-RTT data carries replay risk (ticket theft). Only idempotent methods (`GET`, `HEAD`, `OPTIONS`) are sent in the 0-RTT window. `POST`, `PUT`, `DELETE` wait for the 1-RTT confirmation.

---

### TLS 1.3 & Per-Packet Encryption

**`protocol/src/tls.rs`** — `TlsServerSession`, `TlsClientSession`, `PacketCrypto`

Security operates on two levels:

**1. Handshake (rustls):** `TlsServerSession` and `TlsClientSession` wrap rustls `ServerConnection` / `ClientConnection`. Standard TLS 1.3 ClientHello / ServerHello exchange negotiates cipher suites and derives key material.

**2. Per-packet AEAD (ring):** After the handshake, each UDP packet payload is independently sealed with ChaCha20-Poly1305. This is stateless — packets can be decrypted out of order.

**Nonce construction:**
```
[ 0x00 0x00 0x00 0x00 | packet_num (8 bytes big-endian) ]
                 12 bytes total
```

**Additional Authenticated Data (AAD):**
```
[ conn_id (8B) | flags (1B) | frame_type (1B) ]
```

The AAD binds the ciphertext to its connection and packet type — a packet with modified flags will fail authentication even if the payload is intact.

**Key derivation:** After the TLS handshake completes, both sides derive the symmetric `PacketCrypto` key from TLS exporter material (rustls keying material exporter). In development mode: random 32-byte key via `ring::rand::SystemRandom`.

---

### HPACK Header Compression

**`protocol/src/hpack.rs`** — `HpackEncoder`, `HpackDecoder`

HTTP headers are repetitive — `:method GET`, `:scheme https`, and `content-type application/json` appear on almost every request. HPACK replaces common headers with single-byte table indices.

**Static table:** 61 entries from the HTTP/2 specification (RFC 7541), covering `:method`, `:path`, `:scheme`, `:status`, and all common headers.

**Encoding rules:**

| Prefix | Meaning | Size |
|---|---|---|
| `0x80 \| idx` | Full static table hit (name + value) | 1 byte |
| `0x40 \| idx` + string | Name from table, literal value | 2 + value bytes |
| `0x00` + string + string | Literal name + literal value | 3 + name + value bytes |

**Example:**
```
:method GET   → 0x82   (index 2)         = 1 byte   vs 10 bytes raw
:path /       → 0x84   (index 4)         = 1 byte   vs 7 bytes raw
:scheme https → 0x87   (index 7)         = 1 byte   vs 13 bytes raw
:status 200   → 0x88   (index 8)         = 1 byte   vs 10 bytes raw
```

Typical compression ratio: **40–60% reduction** on request/response headers. This implementation uses the static table only — no dynamic table state — which keeps the encoder and decoder stateless and restart-safe.

---

### Flow Control

**`protocol/src/flow.rs`** — `SendWindow`, `RecvWindow`, `FlowWindows`

Credit-based flow control at two levels, modeled after HTTP/2's window system.

**SendWindow (sender side):**

```
available: u64   ← decrements as bytes are sent
                 ← increments when WINDOW_UPDATE received
can_send(n)     → true if available >= n
consume(n)      → deduct n from available
add_credit(n)   → add n credits (from WINDOW_UPDATE)
```

**RecvWindow (receiver side):**

```
window_size: u64   ← advertised to sender at stream open
received:    u64   ← total bytes received
consumed:    u64   ← bytes drained by application
```

When `consumed >= 50% of initial window`, a `WINDOW_UPDATE` control frame is sent to restore the sender's credit. If `received > window_size`, the packet is rejected as a flow control violation.

**Dual-window enforcement:** Both the stream-level and connection-level windows must be positive before any send is permitted. A stream window being open does not override a depleted connection window.

**`WINDOW_UPDATE` wire format (Control sub-type `0x02`):**
```
[ stream_id (4B) | increment (4B) ]
stream_id = 0  →  connection-level update
stream_id > 0  →  stream-level update
```

---

### Hybrid TCP/UDP Transport

**`protocol/src/hybridProtocol/`** — experimental

Some networks (corporate firewalls, certain ISPs) block UDP entirely. The hybrid transport maintains a TCP control channel alongside the UDP data channel:

- `tcp.rs` — TCP connection for signaling, handshake, and fallback data
- `udp.rs` — UDP connection for bulk data transfer
- `client.rs` — establishes TCP first, negotiates UDP capability, upgrades if available

This allows HTTP/K.0 to degrade gracefully to TCP + TLS when UDP is unavailable, maintaining correctness at the cost of latency.

---

## Packet & Frame Wire Format

### Full packet on the wire

```
┌──────────┬──────────┬──────┬────────┬─────────┬──────────────────────────┐
│ conn_id  │ pkt_num  │flags │  type  │ pay_len │        payload           │
│  8 bytes │  8 bytes │  1B  │   1B   │  4 bytes│  (encrypted after hdshk) │
└──────────┴──────────┴──────┴────────┴─────────┴──────────────────────────┘
                                                  └── ChaCha20-Poly1305 ───┘
```

### Stream frame payload (inside packet)

```
┌───────────┬────────┬─────────────────┐
│ stream_id │ offset │      data       │
│  4 bytes  │ 4 bytes│    N bytes      │
└───────────┴────────┴─────────────────┘
```

### HTTP/K.0 frame (inside stream data)

```
┌──────┬──────────┬──────────────────────────────────┐
│ kind │  length  │             payload               │
│  1B  │  4 bytes │  (HPACK headers / body / etc.)   │
└──────┴──────────┴──────────────────────────────────┘
```

---

## Development Status

| Phase | Feature | Status |
|---|---|---|
| **Phase 1** | Packet codec (22B header, all frame types) | Complete |
| **Phase 1** | ACK tracking & RFC 6298 RTT estimation | Complete |
| **Phase 1** | BBR-Lite congestion control (all 4 phases) | Complete |
| **Phase 1** | XOR FEC (4+1 group, single-loss recovery) | Complete |
| **Phase 2** | TLS 1.3 handshake (rustls) | Complete |
| **Phase 2** | Per-packet ChaCha20-Poly1305 encryption | Complete |
| **Phase 2** | HPACK header compression (static table) | Complete |
| **Phase 2** | Per-stream + connection-level flow control | Complete |
| **Phase 2** | 0-RTT session resumption (24h tickets) | Complete |
| **Phase 2** | HTTP/K.0 framing (8 frame types) | Complete |
| **Phase 3** | Hybrid TCP/UDP fallover transport | In Progress |
| **Phase 3** | Browser engine (HTML parser, DOM, fetch) | Planned |
| **Phase 4** | Stream prioritization | Planned |
| **Phase 4** | HPACK dynamic table | Planned |
| **Phase 4** | Multipath & adaptive congestion | Planned |
| **Phase 4** | GPU rendering (wgpu / iced) | Planned |

---

## Getting Started

### Prerequisites

- Rust toolchain (stable, 2021 edition)
- Tokio async runtime (pulled via Cargo)

### Build

```bash
cd protocol
cargo build --release
```

### Run

```bash
# Start server (binds to 0.0.0.0:9000 by default)
cargo run -- server

# Connect as client (loopback)
cargo run -- client

# Connect to a remote server
cargo run -- client 192.168.1.100:9000

# Run loopback benchmark (measures throughput + RTT)
cargo run -- bench

# Run Phase 2 feature demo (TLS, 0-RTT, flow control, FEC, HPACK)
cargo run -- demo
```

### Test

```bash
cargo test
```

All major modules have unit tests covering encoding/decoding roundtrips, state machine transitions, RTT estimation, congestion phase changes, FEC recovery scenarios, session ticket lifecycle, encryption correctness, and flow control window enforcement.

---

## Roadmap

### Phase 3 — Browser Skeleton
- Minimal CLI browser capable of fetching HTML over HTTP/K.0
- HTML parser (`html5ever`) → DOM tree construction
- Async fetch API using `k0://` URL scheme
- Complete hybrid TCP/UDP fallover

### Phase 4 — Optimization & Streaming
- Stream prioritization (weight + dependency tree)
- HPACK dynamic table for improved compression on repeat headers
- FEC tuning: adaptive group sizes based on observed loss rate
- Multipath support: simultaneous Wi-Fi + cellular paths
- Adaptive congestion: switch between BBR and CUBIC based on path characteristics

### Phase 5 — Rendering & Full Browser
- CSS parser → CSSOM tree
- Render tree construction and text-mode output
- GPU rendering pipeline via `wgpu` / `iced`
- JS engine integration (optional)
- Tabs, address bar, developer tools

---

## Technical Stack

| Component | Library | Notes |
|---|---|---|
| Language | Rust (stable) | Memory safety, zero-cost abstractions |
| Async runtime | tokio (full) | Task scheduling, UDP socket, timers |
| TLS | rustls 0.23 | TLS 1.3, ring backend |
| AEAD encryption | ring 0.17 | ChaCha20-Poly1305, SystemRandom |
| Buffer utilities | bytes | `Bytes` / `BytesMut` for zero-copy |
| Error handling | anyhow | Ergonomic error propagation |
| HTML parsing | html5ever | (planned, Phase 3) |
| Rendering | wgpu / iced | (planned, Phase 5) |
| Serialization | bincode / flatbuffers | (planned) |

---

## References

- [QUIC — RFC 9000](https://www.rfc-editor.org/rfc/rfc9000)
- [HTTP/2 — RFC 7540](https://www.rfc-editor.org/rfc/rfc7540)
- [HPACK — RFC 7541](https://www.rfc-editor.org/rfc/rfc7541)
- [BBR Congestion Control — IETF Draft](https://datatracker.ietf.org/doc/html/draft-cardwell-iccrg-bbr-congestion-control)
- [TLS 1.3 — RFC 8446](https://www.rfc-editor.org/rfc/rfc8446)
- [rustls](https://github.com/rustls/rustls)
- [Tokio](https://tokio.rs)

---

> HTTP/K.0 Browser is experimental software. Phases 1 and 2 of the protocol are complete and tested. Browser engine integration is in active development. Contributions welcome — especially in networking protocols, browser internals, and low-latency systems.
