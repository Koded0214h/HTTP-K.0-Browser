// src/congestion.rs — BBR-lite congestion controller
//
// Full BBR (RFC 9002 style) is complex. This is a simplified BBR that
// captures the key insight: control sending rate based on measured
// bottleneck bandwidth (BW) and min RTT, NOT on packet loss.
//
// This is why it's better than CUBIC for gaming/streaming:
// CUBIC backs off on loss. BBR backs off on delay increase.
// Low-latency streams stay fast even with occasional drops.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

const INITIAL_CWND:   usize = 10;   // packets
const MIN_CWND:       usize = 2;
const MAX_CWND:       usize = 512;
const BW_WINDOW_MS:   u64   = 2_000; // bandwidth sample window (2s)
const MIN_RTT_WINDOW: Duration = Duration::from_secs(10); // min RTT filter window

// BBR phases
#[derive(Debug, Clone, PartialEq)]
pub enum BbrPhase {
    Startup,     // exponential growth to find bottleneck BW
    Drain,       // drain the queue built during startup
    ProbeBw,     // steady state: probe for more BW periodically
    ProbeRtt,    // occasionally drop cwnd to probe min RTT
}

#[derive(Debug)]
struct BwSample {
    bw_bytes_per_ms: f64,
    recorded_at:     Instant,
}

#[derive(Debug)]
pub struct BbrController {
    pub phase:         BbrPhase,
    pub cwnd:          usize,          // congestion window (packets)
    pub pacing_rate:   f64,            // bytes per millisecond

    // bandwidth estimator
    bw_samples:        VecDeque<BwSample>,
    max_bw:            f64,            // peak BW seen in window

    // RTT estimator
    min_rtt:           Duration,
    min_rtt_stamp:     Instant,
    latest_rtt:        Duration,

    // delivery tracking
    delivered:         u64,            // total bytes delivered
    delivered_stamp:   Instant,

    // probe cycle
    probe_cycle:       u8,             // 0-7, pacing gain cycle in ProbeBw
    startup_rounds:    u8,
}

impl BbrController {
    pub fn new() -> Self {
        Self {
            phase:           BbrPhase::Startup,
            cwnd:            INITIAL_CWND,
            pacing_rate:     1.0, // 1 byte/ms initial (will ramp up fast)
            bw_samples:      VecDeque::new(),
            max_bw:          0.0,
            min_rtt:         Duration::from_secs(1),
            min_rtt_stamp:   Instant::now(),
            latest_rtt:      Duration::from_millis(100),
            delivered:       0,
            delivered_stamp: Instant::now(),
            probe_cycle:     0,
            startup_rounds:  0,
        }
    }

    // ── call on every ACK received ────────────────────────────────────────────
    pub fn on_ack(&mut self, bytes_acked: usize, rtt: Duration) {
        self.latest_rtt = rtt;

        // update min RTT (with 10s expiry)
        if rtt < self.min_rtt || self.min_rtt_stamp.elapsed() > MIN_RTT_WINDOW {
            self.min_rtt       = rtt;
            self.min_rtt_stamp = Instant::now();
        }

        // update delivery rate
        let elapsed_ms = self.delivered_stamp.elapsed().as_millis().max(1) as f64;
        self.delivered      += bytes_acked as u64;
        let bw = bytes_acked as f64 / elapsed_ms;
        self.delivered_stamp = Instant::now();

        // record BW sample
        self.bw_samples.push_back(BwSample {
            bw_bytes_per_ms: bw,
            recorded_at: Instant::now(),
        });
        // expire old samples
        let cutoff = Instant::now() - Duration::from_millis(BW_WINDOW_MS);
        while self.bw_samples.front().map_or(false, |s| s.recorded_at < cutoff) {
            self.bw_samples.pop_front();
        }
        // max BW in window
        self.max_bw = self.bw_samples.iter()
            .map(|s| s.bw_bytes_per_ms)
            .fold(0.0_f64, f64::max);

        self.update_phase();
        self.set_cwnd_and_pacing();
    }

    // ── call on packet loss / timeout ─────────────────────────────────────────
    // BBR doesn't react strongly to loss — only ProbeRtt matters
    pub fn on_loss(&mut self) {
        // only halve cwnd if we're still in startup (helps find real capacity)
        if self.phase == BbrPhase::Startup {
            self.cwnd = (self.cwnd / 2).max(MIN_CWND);
        }
        // in steady state, BBR trusts BW estimates over loss signals
    }

    // ── returns whether we can send another packet now ─────────────────────────
    pub fn can_send(&self, in_flight: usize) -> bool {
        in_flight < self.cwnd
    }

    pub fn srtt_ms(&self) -> u64 { self.latest_rtt.as_millis() as u64 }
    pub fn min_rtt_ms(&self) -> u64 { self.min_rtt.as_millis() as u64 }
    pub fn max_bw_kbps(&self) -> f64 { self.max_bw * 8.0 } // bytes/ms → kbps

    // ─────────────────────────────────────────────────────────────────────────
    fn update_phase(&mut self) {
        match self.phase {
            BbrPhase::Startup => {
                self.startup_rounds += 1;
                // exit startup when BW hasn't grown for 3 rounds
                if self.startup_rounds >= 3 && self.max_bw > 0.0 {
                    self.phase = BbrPhase::Drain;
                }
            }
            BbrPhase::Drain => {
                // drain until in-flight ≈ BDP
                // simplified: just switch to ProbeBw after 1 round
                self.phase = BbrPhase::ProbeBw;
                self.probe_cycle = 0;
            }
            BbrPhase::ProbeBw => {
                self.probe_cycle = (self.probe_cycle + 1) % 8;
                // every 10s or so, enter ProbeRtt to refresh min RTT
                if self.min_rtt_stamp.elapsed() > Duration::from_secs(10) {
                    self.phase = BbrPhase::ProbeRtt;
                }
            }
            BbrPhase::ProbeRtt => {
                // stay for ~200ms then go back
                if self.latest_rtt <= self.min_rtt + Duration::from_millis(5) {
                    self.phase = BbrPhase::ProbeBw;
                }
            }
        }
    }

    fn set_cwnd_and_pacing(&mut self) {
        if self.max_bw == 0.0 || self.min_rtt.as_millis() == 0 {
            return;
        }
        // BDP = bandwidth * min_rtt
        let bdp_packets = ((self.max_bw * self.min_rtt.as_millis() as f64) / 1350.0)
            .ceil() as usize;

        let gain = self.pacing_gain();
        self.cwnd = ((bdp_packets as f64 * gain) as usize)
            .max(MIN_CWND)
            .min(MAX_CWND);

        // pacing rate = BW * pacing_gain
        self.pacing_rate = self.max_bw * gain;
    }

    // pacing gain cycle (BBR uses 8-slot cycle for ProbeBw)
    // [1.25, 0.75, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]
    fn pacing_gain(&self) -> f64 {
        match self.phase {
            BbrPhase::Startup  => 2.0,   // exponential probe
            BbrPhase::Drain    => 0.75,  // drain queue
            BbrPhase::ProbeRtt => 0.5,   // probe min RTT with low in-flight
            BbrPhase::ProbeBw  => match self.probe_cycle {
                0 => 1.25, // probe up
                1 => 0.75, // drain
                _ => 1.0,  // cruise
            },
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_in_startup() {
        let c = BbrController::new();
        assert_eq!(c.phase, BbrPhase::Startup);
        assert_eq!(c.cwnd, INITIAL_CWND);
    }

    #[test]
    fn can_send_under_cwnd() {
        let c = BbrController::new();
        assert!(c.can_send(0));
        assert!(c.can_send(INITIAL_CWND - 1));
        assert!(!c.can_send(INITIAL_CWND));
    }

    #[test]
    fn ack_updates_rtt() {
        let mut c = BbrController::new();
        c.on_ack(1350, Duration::from_millis(20));
        assert!(c.min_rtt.as_millis() <= 20);
    }
}