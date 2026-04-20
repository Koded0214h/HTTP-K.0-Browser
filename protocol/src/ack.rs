// src/ack.rs — sliding window ACK engine
//
// Tracks in-flight reliable packets, detects loss via packet-number gaps,
// and drives retransmission. Unreliable streams never enter this tracker.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

const INITIAL_RTO_MS: u64 = 200;   // initial retransmit timeout
const MAX_RTO_MS:     u64 = 5_000; // cap at 5s (exponential backoff)
const MAX_ACK_DELAY:  u64 = 25_000; // max ACK delay we'll tolerate (µs)

#[derive(Debug, Clone)]
pub struct InFlight {
    pub packet_num: u64,
    pub data:       Vec<u8>,   // encoded packet (ready to re-send)
    pub sent_at:    Instant,
    pub retries:    u8,
}

#[derive(Debug)]
pub struct AckTracker {
    // packet_num → in-flight entry (only reliable packets go here)
    pub in_flight: BTreeMap<u64, InFlight>,

    // largest packet number we've ACK'd from the remote
    pub largest_acked: Option<u64>,

    // RTT estimator (EWMA)
    pub smoothed_rtt: Duration,
    pub rtt_var:      Duration,

    // current retransmit timeout
    pub rto: Duration,

    // packets ready for retransmit (populated by tick())
    pub retransmit_queue: VecDeque<Vec<u8>>,

    // ACKs we need to send back (accumulated before flushing)
    pub pending_acks: Vec<u64>,
}

impl AckTracker {
    pub fn new() -> Self {
        Self {
            in_flight:        BTreeMap::new(),
            largest_acked:    None,
            smoothed_rtt:     Duration::from_millis(INITIAL_RTO_MS),
            rtt_var:          Duration::from_millis(50),
            rto:              Duration::from_millis(INITIAL_RTO_MS),
            retransmit_queue: VecDeque::new(),
            pending_acks:     Vec::new(),
        }
    }

    // ── called when we send a reliable packet ─────────────────────────────────
    pub fn track(&mut self, packet_num: u64, encoded: Vec<u8>) {
        self.in_flight.insert(packet_num, InFlight {
            packet_num,
            data: encoded,
            sent_at: Instant::now(),
            retries: 0,
        });
    }

    // ── called when we receive an ACK frame ───────────────────────────────────
    pub fn on_ack(&mut self, acked_pkt: u64, ack_delay_us: u64) {
        if let Some(entry) = self.in_flight.remove(&acked_pkt) {
            let rtt_sample = entry.sent_at.elapsed()
                .saturating_sub(Duration::from_micros(ack_delay_us.min(MAX_ACK_DELAY)));

            // RFC 6298 EWMA update
            let rtt_diff = if rtt_sample > self.smoothed_rtt {
                rtt_sample - self.smoothed_rtt
            } else {
                self.smoothed_rtt - rtt_sample
            };
            self.rtt_var      = (3 * self.rtt_var + rtt_diff) / 4;
            self.smoothed_rtt = (7 * self.smoothed_rtt + rtt_sample) / 8;
            self.rto = (self.smoothed_rtt + 4 * self.rtt_var)
                .max(Duration::from_millis(50))
                .min(Duration::from_millis(MAX_RTO_MS));
        }
        // update largest acked
        self.largest_acked = Some(match self.largest_acked {
            Some(prev) => prev.max(acked_pkt),
            None => acked_pkt,
        });
    }

    // ── called when we receive a NAK (explicit loss signal) ──────────────────
    pub fn on_nak(&mut self, lost_pkt: u64) {
        if let Some(entry) = self.in_flight.get_mut(&lost_pkt) {
            self.retransmit_queue.push_back(entry.data.clone());
            entry.sent_at = Instant::now();
            entry.retries += 1;
        }
    }

    // ── queue an ACK to send back to the remote ───────────────────────────────
    pub fn queue_ack(&mut self, packet_num: u64) {
        self.pending_acks.push(packet_num);
    }

    // ── drain pending ACKs (call this in the send loop) ───────────────────────
    pub fn drain_acks(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.pending_acks)
    }

    // ── called every ~10ms — moves timed-out packets to retransmit queue ─────
    pub fn tick(&mut self) {
        let now = Instant::now();
        for entry in self.in_flight.values_mut() {
            if now.duration_since(entry.sent_at) > self.rto {
                self.retransmit_queue.push_back(entry.data.clone());
                entry.sent_at = now; // reset timer (exponential backoff applied externally)
                entry.retries += 1;
                // double RTO on loss (exponential backoff)
                self.rto = (self.rto * 2).min(Duration::from_millis(MAX_RTO_MS));
            }
        }
    }

    // ── pop next packet to retransmit ─────────────────────────────────────────
    pub fn next_retransmit(&mut self) -> Option<Vec<u8>> {
        self.retransmit_queue.pop_front()
    }

    pub fn in_flight_count(&self) -> usize { self.in_flight.len() }
    pub fn srtt_ms(&self) -> u64 { self.smoothed_rtt.as_millis() as u64 }
    pub fn rto_ms(&self)  -> u64 { self.rto.as_millis() as u64 }
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_and_ack() {
        let mut t = AckTracker::new();
        t.track(0, vec![0xAA; 64]);
        t.track(1, vec![0xBB; 64]);
        assert_eq!(t.in_flight_count(), 2);
        t.on_ack(0, 0);
        assert_eq!(t.in_flight_count(), 1);
        assert_eq!(t.largest_acked, Some(0));
    }

    #[test]
    fn nak_queues_retransmit() {
        let mut t = AckTracker::new();
        t.track(5, vec![0xCC; 32]);
        t.on_nak(5);
        assert!(t.next_retransmit().is_some());
    }

    #[test]
    fn rtt_update_sanity() {
        let mut t = AckTracker::new();
        t.track(0, vec![]);
        std::thread::sleep(Duration::from_millis(10));
        t.on_ack(0, 0);
        // SRTT should have moved from 200ms toward ~10ms
        assert!(t.srtt_ms() < 200);
    }
}