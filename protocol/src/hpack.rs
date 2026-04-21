#![allow(dead_code)]
// src/hpack.rs — Header compression for HTTP/K.0
//
// Full HPACK (RFC 7541) has a dynamic table that both sides maintain in sync.
// That's complex and stateful. We implement HPACK-lite:
//   - Static table (61 entries, same as HTTP/2 HPACK spec)
//   - Literal encoding with Huffman-optional strings (we use raw strings)
//   - No dynamic table (stateless — simpler, still saves 40-60% on common headers)
//
// Why compression matters for browsing:
//   HTTP/1.1: headers sent as plain text EVERY request (~500-800 bytes each)
//   HTTP/2:   HPACK compresses repeated headers to 1-2 bytes via dynamic table
//   HTTP/K.0: HPACK-lite saves the common headers via static table lookup
//
// Wire format per header block:
//   Static table hit:  [1B: 0x80 | index]           (1 byte total!)
//   Literal name+val:  [1B: 0x00][len+name][len+val] (full string)
//   Literal val only:  [1B: 0x40 | index][len+val]  (indexed name, literal val)

// ── Static table (RFC 7541 Appendix A, entries 1-61) ─────────────────────────
// Index 0 is unused (1-based in spec). We store at index-1 internally.

const STATIC_TABLE: &[(&str, &str)] = &[
    (":authority",                   ""),           // 1
    (":method",                      "GET"),        // 2
    (":method",                      "POST"),       // 3
    (":path",                        "/"),          // 4
    (":path",                        "/index.html"),// 5
    (":scheme",                      "http"),       // 6
    (":scheme",                      "https"),      // 7
    (":status",                      "200"),        // 8
    (":status",                      "204"),        // 9
    (":status",                      "206"),        // 10
    (":status",                      "304"),        // 11
    (":status",                      "400"),        // 12
    (":status",                      "404"),        // 13
    (":status",                      "500"),        // 14
    ("accept-charset",               ""),           // 15
    ("accept-encoding",              "gzip, deflate"), // 16
    ("accept-language",              ""),           // 17
    ("accept-ranges",                ""),           // 18
    ("accept",                       ""),           // 19
    ("access-control-allow-origin",  ""),           // 20
    ("age",                          ""),           // 21
    ("allow",                        ""),           // 22
    ("authorization",                ""),           // 23
    ("cache-control",                ""),           // 24
    ("content-disposition",          ""),           // 25
    ("content-encoding",             ""),           // 26
    ("content-language",             ""),           // 27
    ("content-length",               ""),           // 28
    ("content-location",             ""),           // 29
    ("content-range",                ""),           // 30
    ("content-type",                 ""),           // 31
    ("cookie",                       ""),           // 32
    ("date",                         ""),           // 33
    ("etag",                         ""),           // 34
    ("expect",                       ""),           // 35
    ("expires",                      ""),           // 36
    ("from",                         ""),           // 37
    ("host",                         ""),           // 38
    ("if-match",                     ""),           // 39
    ("if-modified-since",            ""),           // 40
    ("if-none-match",                ""),           // 41
    ("if-range",                     ""),           // 42
    ("if-unmodified-since",          ""),           // 43
    ("last-modified",                ""),           // 44
    ("link",                         ""),           // 45
    ("location",                     ""),           // 46
    ("max-forwards",                 ""),           // 47
    ("proxy-authenticate",           ""),           // 48
    ("proxy-authorization",          ""),           // 49
    ("range",                        ""),           // 50
    ("referer",                      ""),           // 51
    ("refresh",                      ""),           // 52
    ("retry-after",                  ""),           // 53
    ("server",                       ""),           // 54
    ("set-cookie",                   ""),           // 55
    ("strict-transport-security",    ""),           // 56
    ("transfer-encoding",            ""),           // 57
    ("user-agent",                   ""),           // 58
    ("vary",                         ""),           // 59
    ("via",                          ""),           // 60
    ("www-authenticate",             ""),           // 61
];

// ── Header type ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub name:  String,
    pub value: String,
}

impl Header {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self { name: name.into(), value: value.into() }
    }
}

// ── Encoder ───────────────────────────────────────────────────────────────────

pub struct HpackEncoder;

impl HpackEncoder {
    pub fn new() -> Self { Self }

    /// Encode a list of headers into a compressed byte block.
    pub fn encode(&self, headers: &[Header]) -> Vec<u8> {
        let mut out = Vec::new();
        for h in headers {
            if let Some(idx) = Self::static_exact(h) {
                // full static table hit — 1 byte! (0x80 | index)
                out.push(0x80 | idx as u8);
            } else if let Some(idx) = Self::static_name(h) {
                // name in static table, literal value
                out.push(0x40 | idx as u8);
                Self::write_string(&mut out, &h.value);
            } else {
                // literal name + value (0x00 prefix)
                out.push(0x00);
                Self::write_string(&mut out, &h.name);
                Self::write_string(&mut out, &h.value);
            }
        }
        out
    }

    fn static_exact(h: &Header) -> Option<usize> {
        STATIC_TABLE.iter().position(|(n, v)| {
            *n == h.name.as_str() && *v == h.value.as_str()
        }).map(|i| i + 1) // 1-based
    }

    fn static_name(h: &Header) -> Option<usize> {
        STATIC_TABLE.iter().position(|(n, _)| {
            *n == h.name.as_str()
        }).map(|i| i + 1)
    }

    fn write_string(out: &mut Vec<u8>, s: &str) {
        // no Huffman for simplicity — bit 7 = 0 means raw string
        let bytes = s.as_bytes();
        out.push(bytes.len() as u8); // length prefix (max 127 bytes per value)
        out.extend_from_slice(bytes);
    }
}

// ── Decoder ───────────────────────────────────────────────────────────────────

pub struct HpackDecoder;

impl HpackDecoder {
    pub fn new() -> Self { Self }

    /// Decode a compressed header block back into headers.
    pub fn decode(&self, src: &[u8]) -> anyhow::Result<Vec<Header>> {
        let mut headers = Vec::new();
        let mut i = 0usize;

        while i < src.len() {
            let byte = src[i];

            if byte & 0x80 != 0 {
                // indexed representation — full static table hit
                let idx = (byte & 0x7F) as usize;
                if idx == 0 || idx > STATIC_TABLE.len() {
                    return Err(anyhow::anyhow!("invalid static index {idx}"));
                }
                let (name, value) = STATIC_TABLE[idx - 1];
                headers.push(Header::new(name, value));
                i += 1;

            } else if byte & 0x40 != 0 {
                // literal with indexed name
                let idx = (byte & 0x3F) as usize;
                i += 1;
                let value = Self::read_string(src, &mut i)?;
                if idx == 0 || idx > STATIC_TABLE.len() {
                    return Err(anyhow::anyhow!("invalid name index {idx}"));
                }
                let (name, _) = STATIC_TABLE[idx - 1];
                headers.push(Header::new(name, value));

            } else {
                // literal name + value
                i += 1; // skip the 0x00 byte
                let name  = Self::read_string(src, &mut i)?;
                let value = Self::read_string(src, &mut i)?;
                headers.push(Header::new(name, value));
            }
        }

        Ok(headers)
    }

    fn read_string(src: &[u8], i: &mut usize) -> anyhow::Result<String> {
        if *i >= src.len() {
            return Err(anyhow::anyhow!("truncated header block"));
        }
        let len = src[*i] as usize;
        *i += 1;
        if *i + len > src.len() {
            return Err(anyhow::anyhow!("header string out of bounds"));
        }
        let s = std::str::from_utf8(&src[*i..*i + len])
            .map_err(|e| anyhow::anyhow!("header not utf8: {e}"))?
            .to_string();
        *i += len;
        Ok(s)
    }
}

// ── K.0 Request / Response types ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct K0Request {
    pub method:  String,
    pub path:    String,
    pub scheme:  String,
    pub headers: Vec<Header>,
    pub body:    Vec<u8>,
}

impl K0Request {
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method:  "GET".into(),
            path:    path.into(),
            scheme:  "k0".into(),
            headers: vec![],
            body:    vec![],
        }
    }

    pub fn post(path: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            method:  "POST".into(),
            path:    path.into(),
            scheme:  "k0".into(),
            headers: vec![],
            body,
        }
    }

    /// Encode to HEADERS frame payload (all pseudo + regular headers compressed).
    pub fn encode_headers(&self) -> Vec<u8> {
        let enc = HpackEncoder::new();
        let mut headers = vec![
            Header::new(":method", &self.method),
            Header::new(":path",   &self.path),
            Header::new(":scheme", &self.scheme),
        ];
        headers.extend(self.headers.clone());
        enc.encode(&headers)
    }
}

#[derive(Debug, Clone)]
pub struct K0Response {
    pub status:  u16,
    pub headers: Vec<Header>,
    pub body:    Vec<u8>,
}

impl K0Response {
    pub fn ok(body: Vec<u8>) -> Self {
        Self { status: 200, headers: vec![], body }
    }

    pub fn not_found() -> Self {
        Self { status: 404, headers: vec![], body: b"Not Found".to_vec() }
    }

    pub fn encode_headers(&self) -> Vec<u8> {
        let enc = HpackEncoder::new();
        let mut headers = vec![
            Header::new(":status", self.status.to_string()),
        ];
        headers.extend(self.headers.clone());
        enc.encode(&headers)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_table_exact_hit() {
        // :method GET is entry 2 → should encode as single byte 0x82
        let enc = HpackEncoder::new();
        let headers = vec![Header::new(":method", "GET")];
        let encoded = enc.encode(&headers);
        assert_eq!(encoded.len(), 1);
        assert_eq!(encoded[0], 0x80 | 2); // index 2
    }

    #[test]
    fn roundtrip_mixed_headers() {
        let enc = HpackEncoder::new();
        let dec = HpackDecoder::new();
        let headers = vec![
            Header::new(":method",  "GET"),
            Header::new(":path",    "/api/v1/data"),
            Header::new(":scheme",  "https"),
            Header::new("content-type", "application/json"),
        ];
        let encoded = enc.encode(&headers);
        let decoded = dec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), headers.len());
        assert_eq!(decoded[0].name,  ":method");
        assert_eq!(decoded[0].value, "GET");
        assert_eq!(decoded[1].value, "/api/v1/data");
        assert_eq!(decoded[3].value, "application/json");
    }

    #[test]
    fn compression_ratio() {
        let enc = HpackEncoder::new();
        // Common browser request headers
        let headers = vec![
            Header::new(":method",          "GET"),
            Header::new(":path",            "/"),
            Header::new(":scheme",          "https"),
            Header::new("accept-encoding",  "gzip, deflate"),
        ];
        let encoded   = enc.encode(&headers);
        let raw_bytes: usize = headers.iter()
            .map(|h| h.name.len() + h.value.len() + 4)
            .sum();
        println!("raw={raw_bytes}B compressed={}B ratio={:.0}%",
            encoded.len(), 100.0 * encoded.len() as f64 / raw_bytes as f64);
        assert!(encoded.len() < raw_bytes, "compression should reduce size");
    }

    #[test]
    fn request_encode_decode() {
        let req = K0Request::get("/index.html");
        let encoded = req.encode_headers();
        let decoded = HpackDecoder::new().decode(&encoded).unwrap();
        let method = decoded.iter().find(|h| h.name == ":method").unwrap();
        let path   = decoded.iter().find(|h| h.name == ":path").unwrap();
        assert_eq!(method.value, "GET");
        assert_eq!(path.value,   "/index.html");
    }
}