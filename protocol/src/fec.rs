// src/fec.rs — XOR parity FEC
//
// Groups N data packets into a block. Computes 1 XOR parity shard.
// If any ONE packet in the group is lost, receiver can reconstruct it
// from the other (N-1) packets + the parity shard — no retransmit needed.
//
// Best for: game state, audio, video — anything where latency > reliability.
// Not for: reliable-ordered streams (use ACK retransmit there instead).

const DEFAULT_GROUP_SIZE: usize = 4; // 4 data + 1 parity per FEC block

#[derive(Debug)]
pub struct FecEncoder {
    pub group_size: usize,
    buffer:         Vec<Vec<u8>>, // accumulated data packets in current block
    max_payload:    usize,
}

impl FecEncoder {
    pub fn new(group_size: usize, max_payload: usize) -> Self {
        Self { group_size, buffer: Vec::new(), max_payload }
    }

    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_GROUP_SIZE, 1350)
    }

    // ── feed a data packet payload. Returns parity shard when block is full ──
    pub fn feed(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        self.buffer.push(data.to_vec());
        if self.buffer.len() >= self.group_size {
            let parity = self.compute_parity();
            self.buffer.clear();
            Some(parity)
        } else {
            None
        }
    }

    // ── force flush partial block (end of stream) ─────────────────────────────
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.buffer.is_empty() { return None; }
        let parity = self.compute_parity();
        self.buffer.clear();
        Some(parity)
    }

    fn compute_parity(&self) -> Vec<u8> {
        // find longest payload in block
        let len = self.buffer.iter().map(|v| v.len()).max().unwrap_or(0);
        let mut parity = vec![0u8; len];
        for packet in &self.buffer {
            for (i, &byte) in packet.iter().enumerate() {
                parity[i] ^= byte;
            }
            // zero-pad shorter packets implicitly (XOR with 0 = no effect)
        }
        parity
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct FecDecoder {
    pub group_size: usize,
    // slot index → payload (None = not received yet)
    received:   Vec<Option<Vec<u8>>>,
    parity:     Option<Vec<u8>>,
}

impl FecDecoder {
    pub fn new(group_size: usize) -> Self {
        Self {
            group_size,
            received: vec![None; group_size],
            parity:   None,
        }
    }

    pub fn with_defaults() -> Self { Self::new(DEFAULT_GROUP_SIZE) }

    // ── receive a data shard (slot 0..group_size-1) ───────────────────────────
    pub fn receive_data(&mut self, slot: usize, data: Vec<u8>) {
        if slot < self.group_size {
            self.received[slot] = Some(data);
        }
    }

    // ── receive the parity shard ──────────────────────────────────────────────
    pub fn receive_parity(&mut self, parity: Vec<u8>) {
        self.parity = Some(parity);
    }

    // ── attempt recovery. Returns recovered shard if exactly 1 slot is missing.
    // Returns None if block is complete (no recovery needed) or unrecoverable.
    pub fn recover(&self) -> FecResult {
        let missing: Vec<usize> = self.received.iter()
            .enumerate()
            .filter(|(_, v)| v.is_none())
            .map(|(i, _)| i)
            .collect();

        match missing.len() {
            0 => FecResult::Complete,
            1 => {
                let parity = match &self.parity {
                    Some(p) => p,
                    None    => return FecResult::Unrecoverable,
                };
                let lost_slot = missing[0];
                let len = parity.len();
                let mut recovered = parity.clone();
                for (i, slot) in self.received.iter().enumerate() {
                    if i == lost_slot { continue; }
                    if let Some(data) = slot {
                        for (j, &byte) in data.iter().enumerate() {
                            if j < len { recovered[j] ^= byte; }
                        }
                    }
                }
                FecResult::Recovered { slot: lost_slot, data: recovered }
            }
            _ => FecResult::Unrecoverable,
        }
    }

    pub fn reset(&mut self) {
        self.received = vec![None; self.group_size];
        self.parity   = None;
    }
}

#[derive(Debug)]
pub enum FecResult {
    Complete,
    Recovered { slot: usize, data: Vec<u8> },
    Unrecoverable,
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_no_loss() {
        let mut enc = FecEncoder::new(4, 1350);
        let packets = vec![b"aaaa".to_vec(), b"bbbb".to_vec(),
                           b"cccc".to_vec(), b"dddd".to_vec()];
        let mut parity_out = None;
        for p in &packets { parity_out = enc.feed(p); }
        let parity = parity_out.unwrap();

        let mut dec = FecDecoder::new(4);
        for (i, p) in packets.iter().enumerate() { dec.receive_data(i, p.clone()); }
        dec.receive_parity(parity);
        assert!(matches!(dec.recover(), FecResult::Complete));
    }

    #[test]
    fn recover_single_loss() {
        let mut enc = FecEncoder::new(4, 1350);
        let packets = vec![
            vec![0x01u8, 0x02, 0x03, 0x04],
            vec![0x10u8, 0x20, 0x30, 0x40],
            vec![0xAAu8, 0xBB, 0xCC, 0xDD],
            vec![0x11u8, 0x22, 0x33, 0x44],
        ];
        let mut parity_out = None;
        for p in &packets { parity_out = enc.feed(p); }
        let parity = parity_out.unwrap();

        // drop packet 2
        let mut dec = FecDecoder::new(4);
        dec.receive_data(0, packets[0].clone());
        dec.receive_data(1, packets[1].clone());
        // slot 2 is lost
        dec.receive_data(3, packets[3].clone());
        dec.receive_parity(parity);

        match dec.recover() {
            FecResult::Recovered { slot, data } => {
                assert_eq!(slot, 2);
                assert_eq!(data, packets[2]);
            }
            other => panic!("Expected recovery, got {:?}", other),
        }
    }

    #[test]
    fn unrecoverable_two_losses() {
        let mut dec = FecDecoder::new(4);
        dec.receive_parity(vec![0u8; 4]);
        // only 2 of 4 slots filled → unrecoverable
        dec.receive_data(0, vec![1, 2, 3, 4]);
        assert!(matches!(dec.recover(), FecResult::Unrecoverable));
    }
}