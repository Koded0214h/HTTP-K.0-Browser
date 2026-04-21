#![allow(dead_code)]
// src/tls.rs — TLS 1.3 layer for HTTP/K.0
//
// How it fits:
//   Handshake payloads carry TLS ClientHello/ServerHello bytes.
//   After handshake completes, each packet payload is encrypted with
//   ChaCha20-Poly1305 (per-packet AEAD, not TLS record framing).
//
// Wire:  [conn_id][pkt_num][flags][frame_type] stay plaintext for routing.
//        [payload] is encrypted after handshake.

use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection, RootCertStore};
use rustls_pemfile::{certs, private_key};
use ring::rand::{SecureRandom, SystemRandom};
use ring::aead::{LessSafeKey, UnboundKey, Nonce, Aad, CHACHA20_POLY1305};
use std::io::{BufReader, Cursor, Read, Write};
use std::sync::Arc;

// ── TLS config builders ───────────────────────────────────────────────────────

pub fn make_server_config(
    cert_pem: &[u8],
    key_pem:  &[u8],
) -> anyhow::Result<Arc<ServerConfig>> {
    let cert_chain: Vec<CertificateDer<'static>> = {
        let mut r = BufReader::new(cert_pem);
        certs(&mut r).collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("cert parse: {e}"))?
    };
    let key = {
        let mut r = BufReader::new(key_pem);
        private_key(&mut r)?
            .ok_or_else(|| anyhow::anyhow!("no private key in PEM"))?
    };
    let cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)?;
    Ok(Arc::new(cfg))
}

pub fn make_client_config(server_cert_pem: &[u8]) -> anyhow::Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    let mut r = BufReader::new(server_cert_pem);
    for cert in certs(&mut r) {
        let cert = cert.map_err(|e| anyhow::anyhow!("{e}"))?;
        roots.add(cert)?;
    }
    let cfg = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

// ── Server TLS session ────────────────────────────────────────────────────────

pub struct TlsServerSession { conn: ServerConnection }

impl TlsServerSession {
    pub fn new(config: Arc<ServerConfig>) -> anyhow::Result<Self> {
        Ok(Self { conn: ServerConnection::new(config)? })
    }

    pub fn read_tls(&mut self, data: &[u8]) -> anyhow::Result<()> {
        let mut c = Cursor::new(data);
        self.conn.read_tls(&mut c)?;
        self.conn.process_new_packets()
            .map_err(|e| anyhow::anyhow!("TLS: {e}"))?;
        Ok(())
    }

    pub fn write_tls(&mut self) -> anyhow::Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.conn.write_tls(&mut buf)?;
        Ok(buf)
    }

    pub fn is_handshake_complete(&self) -> bool { !self.conn.is_handshaking() }

    pub fn encrypt(&mut self, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.conn.writer().write_all(plaintext)?;
        self.write_tls()
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.read_tls(ciphertext)?;
        let mut out = Vec::new();
        self.conn.reader().read_to_end(&mut out)?;
        Ok(out)
    }
}

// ── Client TLS session ────────────────────────────────────────────────────────

pub struct TlsClientSession { conn: ClientConnection }

impl TlsClientSession {
    pub fn new(config: Arc<ClientConfig>, server_name: &str) -> anyhow::Result<Self> {
        let name: ServerName<'static> = ServerName::try_from(server_name.to_string())
            .map_err(|e| anyhow::anyhow!("bad server name: {e}"))?;
        Ok(Self { conn: ClientConnection::new(config, name)? })
    }

    pub fn read_tls(&mut self, data: &[u8]) -> anyhow::Result<()> {
        let mut c = Cursor::new(data);
        self.conn.read_tls(&mut c)?;
        self.conn.process_new_packets()
            .map_err(|e| anyhow::anyhow!("TLS: {e}"))?;
        Ok(())
    }

    pub fn write_tls(&mut self) -> anyhow::Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.conn.write_tls(&mut buf)?;
        Ok(buf)
    }

    pub fn is_handshake_complete(&self) -> bool { !self.conn.is_handshaking() }

    pub fn initial_handshake_bytes(&mut self) -> anyhow::Result<Vec<u8>> {
        self.write_tls()
    }

    pub fn encrypt(&mut self, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.conn.writer().write_all(plaintext)?;
        self.write_tls()
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.read_tls(ciphertext)?;
        let mut out = Vec::new();
        self.conn.reader().read_to_end(&mut out)?;
        Ok(out)
    }
}

// ── Per-packet AEAD (ChaCha20-Poly1305) ──────────────────────────────────────
// Encrypts each UDP packet payload independently.
// Nonce:  [4B zeros][8B packet_num]  — unique per packet on this connection
// AAD:    [8B conn_id][1B flags][1B frame_type] — authenticated, not encrypted

pub struct PacketCrypto { seal_key: LessSafeKey }

impl PacketCrypto {
    pub fn new(key_bytes: &[u8; 32]) -> anyhow::Result<Self> {
        let unbound = UnboundKey::new(&CHACHA20_POLY1305, key_bytes)
            .map_err(|e| anyhow::anyhow!("key: {e}"))?;
        Ok(Self { seal_key: LessSafeKey::new(unbound) })
    }

    pub fn random_key() -> anyhow::Result<[u8; 32]> {
        let rng = SystemRandom::new();
        let mut k = [0u8; 32];
        rng.fill(&mut k).map_err(|e| anyhow::anyhow!("rng: {e}"))?;
        Ok(k)
    }

    pub fn encrypt(&self, packet_num: u64, aad: &[u8], plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut buf = plaintext.to_vec();
        self.seal_key
            .seal_in_place_append_tag(Self::nonce(packet_num), Aad::from(aad), &mut buf)
            .map_err(|e| anyhow::anyhow!("seal: {e}"))?;
        Ok(buf)
    }

    pub fn decrypt<'a>(&self, packet_num: u64, aad: &[u8], ct: &'a mut Vec<u8>) -> anyhow::Result<&'a [u8]> {
        self.seal_key
            .open_in_place(Self::nonce(packet_num), Aad::from(aad), ct.as_mut_slice()).map(|s| s as &[u8])
            .map_err(|_| anyhow::anyhow!("decrypt failed — tampered or wrong key"))
    }

    fn nonce(packet_num: u64) -> Nonce {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&packet_num.to_be_bytes());
        Nonce::assume_unique_for_key(n)
    }
}

// ── TLS handshake state ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TlsState {
    None,
    Handshaking,
    Established,
    Failed(String),
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_crypto_roundtrip() {
        let key = PacketCrypto::random_key().unwrap();
        let enc = PacketCrypto::new(&key).unwrap();
        let dec = PacketCrypto::new(&key).unwrap();
        let pt  = b"hello K.0 encrypted world";
        let aad = b"\x00\x00\x00\x00\x00\x00\x00\x01\x00\x10";
        let mut ct = enc.encrypt(42, aad, pt).unwrap();
        assert_ne!(ct, pt.as_ref());
        let recovered = dec.decrypt(42, aad, &mut ct).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn wrong_packet_num_fails() {
        let key = PacketCrypto::random_key().unwrap();
        let enc = PacketCrypto::new(&key).unwrap();
        let dec = PacketCrypto::new(&key).unwrap();
        let aad = b"\x00\x00\x00\x00\x00\x00\x00\x01\x00\x10";
        let mut ct = enc.encrypt(1, aad, b"secret").unwrap();
        assert!(dec.decrypt(2, aad, &mut ct).is_err());
    }

    #[test]
    fn nonces_unique_per_packet() {
        let n1 = PacketCrypto::nonce(0);
        let n2 = PacketCrypto::nonce(1);
        assert_ne!(n1.as_ref(), n2.as_ref());
    }
}