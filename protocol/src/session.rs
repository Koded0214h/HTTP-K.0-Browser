#![allow(dead_code)]
// src/session.rs — 0-RTT session resumption
//
// Normal K.0 handshake costs 1 round trip (INIT → INIT_ACK → DONE).
// With TLS that's 1-2 RTTs for the TLS negotiation on top.
// For repeat connections to the same server (browser revisiting a site),
// we can skip all of that.
//
// How 0-RTT works:
//   First visit:
//     - Complete full handshake
//     - Server issues a SessionTicket (encrypted blob with session keys)
//     - Client stores it locally (our SessionStore)
//
//   Second visit:
//     - Client sends INIT packet with SessionTicket in payload
//     - Server decrypts ticket, verifies it's not expired, recovers keys
//     - Server immediately accepts connection — no round trip needed
//     - Client can start sending HEADERS/DATA in the same first packet
//
// Security notes:
//   - 0-RTT data is NOT forward secret (replayed if ticket is stolen)
//   - We only allow safe/idempotent requests in 0-RTT (GET, HEAD, OPTIONS)
//   - POST/PUT/DELETE must wait for 1-RTT confirmation
//   - Tickets expire after SESSION_LIFETIME_SECS

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use ring::rand::{SecureRandom, SystemRandom};

const SESSION_LIFETIME_SECS: u64 = 86_400; // 24 hours
const TICKET_KEY_LEN:         usize = 32;
const MAX_CACHED_SESSIONS:    usize = 1024;

// ── Session ticket (issued by server, stored by client) ───────────────────────

#[derive(Debug, Clone)]
pub struct SessionTicket {
    /// Opaque encrypted blob (in production: encrypt with server's ticket key)
    pub data:       Vec<u8>,
    /// When this ticket was issued (Unix timestamp)
    pub issued_at:  u64,
    /// Which server this ticket is for
    pub server_id:  String,
    /// Negotiated ALPN protocol
    pub protocol:   String,
}

impl SessionTicket {
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.issued_at) > SESSION_LIFETIME_SECS
    }

    pub fn age_secs(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.issued_at)
    }

    /// Encode to bytes for transmission in INIT packet payload.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        // [8B issued_at][2B server_id len][server_id][2B proto len][proto][data...]
        buf.extend_from_slice(&self.issued_at.to_be_bytes());
        buf.extend_from_slice(&(self.server_id.len() as u16).to_be_bytes());
        buf.extend_from_slice(self.server_id.as_bytes());
        buf.extend_from_slice(&(self.protocol.len() as u16).to_be_bytes());
        buf.extend_from_slice(self.protocol.as_bytes());
        buf.extend_from_slice(&self.data);
        buf
    }

    pub fn decode(raw: &[u8]) -> Option<Self> {
        if raw.len() < 12 { return None; }
        let issued_at = u64::from_be_bytes(raw[0..8].try_into().ok()?);
        let sid_len   = u16::from_be_bytes(raw[8..10].try_into().ok()?) as usize;
        let server_id = String::from_utf8(raw[10..10 + sid_len].to_vec()).ok()?;
        let off       = 10 + sid_len;
        let proto_len = u16::from_be_bytes(raw[off..off + 2].try_into().ok()?) as usize;
        let protocol  = String::from_utf8(raw[off + 2..off + 2 + proto_len].to_vec()).ok()?;
        let data      = raw[off + 2 + proto_len..].to_vec();
        Some(Self { data, issued_at, server_id, protocol })
    }
}

// ── Client-side session store ─────────────────────────────────────────────────
// Caches tickets by server host. Evicts expired tickets automatically.

#[derive(Debug, Default)]
pub struct SessionStore {
    tickets: HashMap<String, SessionTicket>, // server_id → ticket
}

impl SessionStore {
    pub fn new() -> Self { Self::default() }

    /// Store a ticket after a successful full handshake.
    pub fn store(&mut self, server_id: String, ticket: SessionTicket) {
        // evict if over capacity
        if self.tickets.len() >= MAX_CACHED_SESSIONS {
            self.evict_expired();
            // if still full, evict oldest
            if self.tickets.len() >= MAX_CACHED_SESSIONS {
                if let Some(oldest) = self.tickets.keys()
                    .min_by_key(|k| self.tickets[*k].issued_at)
                    .cloned()
                {
                    self.tickets.remove(&oldest);
                }
            }
        }
        self.tickets.insert(server_id, ticket);
    }

    /// Retrieve a valid (non-expired) ticket for a server.
    pub fn get(&self, server_id: &str) -> Option<&SessionTicket> {
        self.tickets.get(server_id).filter(|t| !t.is_expired())
    }

    /// Returns true if we have a valid ticket → can attempt 0-RTT.
    pub fn can_resume(&self, server_id: &str) -> bool {
        self.get(server_id).is_some()
    }

    /// Remove expired tickets.
    pub fn evict_expired(&mut self) {
        self.tickets.retain(|_, t| !t.is_expired());
    }

    pub fn count(&self) -> usize { self.tickets.len() }
}

// ── Server-side ticket issuer ─────────────────────────────────────────────────
// Generates and validates session tickets.
// In production: encrypt ticket data with AES-GCM using a rotating server key.
// Here: we store session state in a HashMap (in-memory, same process).

pub struct TicketIssuer {
    /// Server's ticket signing/encryption key (rotated periodically)
    ticket_key: [u8; TICKET_KEY_LEN],
    /// In-memory session state for validation
    sessions:   HashMap<Vec<u8>, StoredSession>,
    rng:        SystemRandom,
}

#[derive(Debug, Clone)]
struct StoredSession {
    server_id:  String,
    protocol:   String,
    issued_at:  u64,
    /// Derived session keys (would be TLS exporter material in production)
    keys:       Vec<u8>,
}

impl TicketIssuer {
    pub fn new() -> anyhow::Result<Self> {
        let rng = SystemRandom::new();
        let mut ticket_key = [0u8; TICKET_KEY_LEN];
        rng.fill(&mut ticket_key)
            .map_err(|e| anyhow::anyhow!("rng: {e}"))?;
        Ok(Self { ticket_key, sessions: HashMap::new(), rng })
    }

    /// Issue a ticket after a successful full handshake.
    pub fn issue(
        &mut self,
        server_id: &str,
        protocol:  &str,
        keys:      Vec<u8>,
    ) -> anyhow::Result<SessionTicket> {
        let issued_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // generate a random ticket ID
        let mut ticket_id = vec![0u8; 16];
        self.rng.fill(&mut ticket_id)
            .map_err(|e| anyhow::anyhow!("rng: {e}"))?;

        self.sessions.insert(ticket_id.clone(), StoredSession {
            server_id: server_id.to_string(),
            protocol:  protocol.to_string(),
            issued_at,
            keys,
        });

        // evict expired sessions
        self.sessions.retain(|_, s| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now.saturating_sub(s.issued_at) <= SESSION_LIFETIME_SECS
        });

        Ok(SessionTicket {
            data:      ticket_id, // opaque to client
            issued_at,
            server_id: server_id.to_string(),
            protocol:  protocol.to_string(),
        })
    }

    /// Validate a ticket from a client attempting 0-RTT.
    pub fn validate(&self, ticket: &SessionTicket) -> Option<ResumedSession> {
        if ticket.is_expired() {
            return None;
        }
        let stored = self.sessions.get(&ticket.data)?;
        if stored.server_id != ticket.server_id {
            return None;
        }
        Some(ResumedSession {
            server_id: stored.server_id.clone(),
            protocol:  stored.protocol.clone(),
            keys:      stored.keys.clone(),
            age_secs:  ticket.age_secs(),
        })
    }
}

/// A successfully resumed session — no handshake needed.
#[derive(Debug, Clone)]
pub struct ResumedSession {
    pub server_id: String,
    pub protocol:  String,
    pub keys:      Vec<u8>,
    pub age_secs:  u64,
}

// ── 0-RTT safety check ────────────────────────────────────────────────────────
// Only safe (idempotent) methods can be sent in 0-RTT data.
// POST/PUT/DELETE must wait for 1-RTT confirmation to prevent replay attacks.

pub fn is_safe_for_zero_rtt(method: &str) -> bool {
    matches!(method.to_uppercase().as_str(), "GET" | "HEAD" | "OPTIONS" | "TRACE")
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_encode_decode() {
        let t = SessionTicket {
            data:      vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            issued_at: 1_700_000_000,
            server_id: "example.com".into(),
            protocol:  "k0/1.0".into(),
        };
        let encoded = t.encode();
        let decoded = SessionTicket::decode(&encoded).unwrap();
        assert_eq!(decoded.server_id, "example.com");
        assert_eq!(decoded.protocol,  "k0/1.0");
        assert_eq!(decoded.issued_at, 1_700_000_000);
        assert_eq!(decoded.data,      t.data);
    }

    #[test]
    fn session_store_retrieves_valid() {
        let mut store = SessionStore::new();
        let ticket = SessionTicket {
            data:      vec![0u8; 16],
            issued_at: SystemTime::now()
                .duration_since(UNIX_EPOCH).unwrap().as_secs(),
            server_id: "k0.dev".into(),
            protocol:  "k0/1.0".into(),
        };
        store.store("k0.dev".into(), ticket);
        assert!(store.can_resume("k0.dev"));
        assert!(!store.can_resume("other.com"));
    }

    #[test]
    fn expired_ticket_not_returned() {
        let mut store = SessionStore::new();
        let ticket = SessionTicket {
            data:      vec![0u8; 16],
            issued_at: 0, // Unix epoch = very expired
            server_id: "old.com".into(),
            protocol:  "k0/1.0".into(),
        };
        store.store("old.com".into(), ticket);
        assert!(!store.can_resume("old.com"));
    }

    #[test]
    fn ticket_issuer_roundtrip() {
        let mut issuer = TicketIssuer::new().unwrap();
        let keys = vec![0xABu8; 32];
        let ticket = issuer.issue("myserver.com", "k0/1.0", keys.clone()).unwrap();
        let resumed = issuer.validate(&ticket).unwrap();
        assert_eq!(resumed.server_id, "myserver.com");
        assert_eq!(resumed.keys,      keys);
    }

    #[test]
    fn zero_rtt_safety() {
        assert!( is_safe_for_zero_rtt("GET"));
        assert!( is_safe_for_zero_rtt("HEAD"));
        assert!(!is_safe_for_zero_rtt("POST"));
        assert!(!is_safe_for_zero_rtt("DELETE"));
        assert!(!is_safe_for_zero_rtt("PUT"));
    }
}