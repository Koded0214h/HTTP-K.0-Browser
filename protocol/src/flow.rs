#![allow(dead_code)]
// src/flow.rs — Per-stream flow control
//
// Problem without flow control:
//   Server sends 10GB response. Client is slow (mobile, old CPU).
//   Without backpressure, server fills client's recv_buf → OOM crash.
//
// Solution: credit-based flow control (same model as HTTP/2, TCP windows).
//   - Receiver grants N bytes of credit to the sender ("you can send N more bytes")
//   - Sender tracks remaining credit. When it hits 0, it stops sending.
//   - Receiver processes data and issues WINDOW_UPDATE to grant more credit.
//   - This is a cooperative push/pull: receiver controls the rate.
//
// In K.0:
//   - Each stream has its own flow control window (per-stream)
//   - There's also a connection-level window (all streams combined)
//   - Both must have credit for a send to proceed
//   - Window updates are sent as CONTROL frames with a sub-type

const INITIAL_WINDOW: u64  = 64 * 1024;        // 64KB default initial window
const MAX_WINDOW:     u64  = 16 * 1024 * 1024; // 16MB max (don't let it go wild)
const UPDATE_THRESHOLD: f64 = 0.5;             // send window update at 50% consumed

// ── Send-side window (tracks how much we're allowed to send) ─────────────────

#[derive(Debug, Clone)]
pub struct SendWindow {
    /// Bytes we are currently allowed to send (decrements as we send)
    pub available: u64,
    /// Total bytes we've sent (for metrics)
    pub total_sent: u64,
}

impl SendWindow {
    pub fn new() -> Self {
        Self { available: INITIAL_WINDOW, total_sent: 0 }
    }

    pub fn with_initial(n: u64) -> Self {
        Self { available: n, total_sent: 0 }
    }

    /// Can we send `bytes` right now?
    pub fn can_send(&self, bytes: usize) -> bool {
        bytes as u64 <= self.available
    }

    /// Consume credit when sending. Returns false if insufficient credit.
    pub fn consume(&mut self, bytes: usize) -> bool {
        if bytes as u64 > self.available { return false; }
        self.available  -= bytes as u64;
        self.total_sent += bytes as u64;
        true
    }

    /// Receive a WINDOW_UPDATE from the peer — they're giving us more credit.
    pub fn add_credit(&mut self, increment: u64) -> anyhow::Result<()> {
        let new_val = self.available.checked_add(increment)
            .ok_or_else(|| anyhow::anyhow!("window overflow"))?;
        if new_val > MAX_WINDOW {
            return Err(anyhow::anyhow!("window exceeded max {MAX_WINDOW}"));
        }
        self.available = new_val;
        Ok(())
    }

    pub fn is_blocked(&self) -> bool { self.available == 0 }
}

// ── Recv-side window (tracks how much buffer we have, issues updates) ─────────

#[derive(Debug, Clone)]
pub struct RecvWindow {
    /// Total window size we've advertised to the sender
    pub window_size: u64,
    /// Bytes received but not yet consumed by the application
    pub received:    u64,
    /// Bytes consumed by the application (via drain)
    pub consumed:    u64,
    /// Bytes we've already sent WINDOW_UPDATE for
    pub updated:     u64,
}

impl RecvWindow {
    pub fn new() -> Self {
        Self {
            window_size: INITIAL_WINDOW,
            received:    0,
            consumed:    0,
            updated:     0,
        }
    }

    /// Record that we received `bytes` from the peer.
    pub fn on_receive(&mut self, bytes: usize) -> anyhow::Result<()> {
        let new_received = self.received + bytes as u64;
        if new_received > self.window_size {
            return Err(anyhow::anyhow!(
                "peer violated flow control: received {} bytes but window is {}",
                new_received, self.window_size
            ));
        }
        self.received = new_received;
        Ok(())
    }

    /// Application consumed `bytes` from the recv buffer (via drain).
    pub fn on_consume(&mut self, bytes: usize) {
        self.consumed += bytes as u64;
    }

    /// Returns Some(increment) if we should send a WINDOW_UPDATE now.
    /// We send an update when the peer has consumed enough that the window
    /// would shrink below UPDATE_THRESHOLD of the initial size.
    pub fn should_update(&self) -> Option<u64> {
        let consumed_not_updated = self.consumed.saturating_sub(self.updated);
        let threshold = (INITIAL_WINDOW as f64 * UPDATE_THRESHOLD) as u64;
        if consumed_not_updated >= threshold {
            Some(consumed_not_updated)
        } else {
            None
        }
    }

    /// Mark that we've sent a WINDOW_UPDATE for `increment` bytes.
    pub fn mark_updated(&mut self, increment: u64) {
        self.updated     += increment;
        self.window_size += increment; // expand what we've advertised
    }

    pub fn available_recv(&self) -> u64 {
        self.window_size.saturating_sub(self.received)
    }
}

// ── Connection-level flow control ─────────────────────────────────────────────
// All streams share a connection window. A send must have credit in BOTH
// the stream window AND the connection window.

#[derive(Debug)]
pub struct ConnectionFlowControl {
    pub send: SendWindow,
    pub recv: RecvWindow,
}

impl ConnectionFlowControl {
    pub fn new() -> Self {
        Self {
            send: SendWindow::new(),
            recv: RecvWindow::new(),
        }
    }
}

// ── Per-stream flow control ────────────────────────────────────────────────────

#[derive(Debug)]
pub struct StreamFlowControl {
    pub stream_id: u32,
    pub send:      SendWindow,
    pub recv:      RecvWindow,
}

impl StreamFlowControl {
    pub fn new(stream_id: u32) -> Self {
        Self {
            stream_id,
            send: SendWindow::new(),
            recv: RecvWindow::new(),
        }
    }

    /// Check both stream + connection window before sending.
    pub fn can_send(&self, bytes: usize, conn: &ConnectionFlowControl) -> bool {
        self.send.can_send(bytes) && conn.send.can_send(bytes)
    }

    /// Consume from both stream + connection window.
    pub fn consume_send(&mut self, bytes: usize, conn: &mut ConnectionFlowControl) -> bool {
        if !self.can_send(bytes, conn) { return false; }
        self.send.consume(bytes);
        conn.send.consume(bytes);
        true
    }
}

// ── WINDOW_UPDATE frame payload ───────────────────────────────────────────────
// Sent as a CONTROL frame sub-type.
// [4B stream_id (0 = connection level)][4B increment]

pub struct WindowUpdate {
    pub stream_id: u32, // 0 = connection-level
    pub increment: u32,
}

impl WindowUpdate {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8);
        buf.extend_from_slice(&self.stream_id.to_be_bytes());
        buf.extend_from_slice(&self.increment.to_be_bytes());
        buf
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < 8 { return None; }
        let stream_id = u32::from_be_bytes(data[0..4].try_into().ok()?);
        let increment = u32::from_be_bytes(data[4..8].try_into().ok()?);
        Some(Self { stream_id, increment })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_window_blocks_at_zero() {
        let mut w = SendWindow::with_initial(100);
        assert!(w.can_send(100));
        assert!(w.consume(100));
        assert!(w.is_blocked());
        assert!(!w.can_send(1));
    }

    #[test]
    fn credit_restores_window() {
        let mut w = SendWindow::with_initial(100);
        w.consume(100);
        w.add_credit(64 * 1024).unwrap();
        assert!(w.can_send(1000));
    }

    #[test]
    fn recv_window_violation_detected() {
        let mut w = RecvWindow::new();
        // try to receive more than the window allows
        let result = w.on_receive(INITIAL_WINDOW as usize + 1);
        assert!(result.is_err());
    }

    #[test]
    fn window_update_trigger() {
        let mut w = RecvWindow::new();
        // receive then consume 50% of the window
        let half = (INITIAL_WINDOW / 2) as usize;
        w.on_receive(half).unwrap();
        w.on_consume(half);
        // should trigger an update
        assert!(w.should_update().is_some());
    }

    #[test]
    fn stream_checks_both_windows() {
        let mut stream = StreamFlowControl::new(0);
        let mut conn   = ConnectionFlowControl::new();
        // exhaust connection window
        conn.send.consume(INITIAL_WINDOW as usize);
        // stream window still has credit but conn doesn't
        assert!(!stream.can_send(1, &conn));
    }

    #[test]
    fn window_update_encode_decode() {
        let wu = WindowUpdate { stream_id: 3, increment: 32768 };
        let encoded = wu.encode();
        let decoded = WindowUpdate::decode(&encoded).unwrap();
        assert_eq!(decoded.stream_id, 3);
        assert_eq!(decoded.increment, 32768);
    }
}