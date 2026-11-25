//! Microsoft Edge Read Aloud (online neural TTS) over WebSocket.
//!
//! Protocol and Sec-MS-GEC v2 DRM token logic ported from `rany2/edge-tts` (LGPL-3.0,
//! https://github.com/rany2/edge-tts). Requests are serialized through one gate with a
//! cooldown on HTTP 429 so we do not hammer Microsoft's unofficial endpoint.
use crate::net;
use anyhow::{anyhow, Result};
use base64::Engine;
use once_cell::sync::Lazy;
use rustls_pki_types::ServerName;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::TlsConnector;

const HOST: &str = "speech.platform.bing.com";
const PATH_BASE: &str = "/consumer/speech/synthesize/readaloud/edge/v1";
const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const CHROMIUM_MAJOR_VERSION: &str = "143";
const SEC_MS_GEC_VERSION: &str = "2-143.0.3650.75";
const ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";
const OUTPUT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";

const WIN_EPOCH: i64 = 11_644_473_600;
const COOLDOWN_SECS: i64 = 20;

/// Serialises Edge TTS requests (one at a time, avoiding 429 rate-limits).
static GATE: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
/// Unix timestamp (seconds) until which requests fail fast after a 429.
static COOLDOWN_UNTIL: AtomicI64 = AtomicI64::new(0);
/// Server-client clock skew in whole seconds, updated from HTTP 403 `Date` headers.
static CLOCK_SKEW: AtomicI64 = AtomicI64::new(0);

static TLS_CONFIG: Lazy<Arc<rustls::ClientConfig>> = Lazy::new(|| {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("rustls default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(cfg)
});

// ---- Sec-MS-GEC v2 DRM token (ported from rany2/edge-tts drm.py, LGPL-3.0) -----------------------

const SHA256_INITIAL: [u32; 8] = [
    0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A,
    0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19,
];
const SHA256_STATE: [u32; 8] = [
    0xFD9FBEA2, 0x91243CE7, 0xBE499094, 0xCD2E1E0A,
    0x6C006F49, 0x04BD35AB, 0xFEB0A7E4, 0x03EFFBD2,
];
const SHA256_SCHEDULE: [u32; 14] = [
    0xEB24BF9E, 0x1558691F, 0xA0C09EE2, 0x0CFA32BA,
    0xA622EE8C, 0xD1DD9491, 0xC0DD25A0, 0xF9E70AB6,
    0xF0C31573, 0x7058CE4E, 0x7188E606, 0x00E5AB33,
    0xD258B060, 0x72766564,
];
const SHA256_ROUND_CONSTANTS: [u32; 64] = [
    0x428A2F98, 0x71374491, 0xB5C0FBCF, 0xE9B5DBA5, 0x3956C25B, 0x59F111F1, 0x923F82A4, 0xAB1C5ED5,
    0xD807AA98, 0x12835B01, 0x243185BE, 0x550C7DC3, 0x72BE5D74, 0x80DEB1FE, 0x9BDC06A7, 0xC19BF174,
    0xE49B69C1, 0xEFBE4786, 0x0FC19DC6, 0x240CA1CC, 0x2DE92C6F, 0x4A7484AA, 0x5CB0A9DC, 0x76F988DA,
    0x983E5152, 0xA831C66D, 0xB00327C8, 0xBF597FC7, 0xC6E00BF3, 0xD5A79147, 0x06CA6351, 0x14292967,
    0x27B70A85, 0x2E1B2138, 0x4D2C6DFC, 0x53380D13, 0x650A7354, 0x766A0ABB, 0x81C2C92E, 0x92722C85,
    0xA2BFE8A1, 0xA81A664B, 0xC24B8B70, 0xC76C51A3, 0xD192E819, 0xD6990624, 0xF40E3585, 0x106AA070,
    0x19A4C116, 0x1E376C08, 0x2748774C, 0x34B0BCB5, 0x391C0CB3, 0x4ED8AA4A, 0x5B9CCA4F, 0x682E6FF3,
    0x748F82EE, 0x78A5636F, 0x84C87814, 0x8CC70208, 0x90BEFFFA, 0xA4506CEB, 0xBEF9A3F7, 0xC67178F2,
];
const SHA256_ADJUSTMENT: u32 = 0xC9DC06A7;
const SHA256_SIGMA0_ADJUSTMENT: u32 = 0x059C0B80;
const SHA256_SIGMA1_ADJUSTMENT: u32 = 0x000B92C0;

const CS64_COEFFICIENTS: [(u32, u32); 14] = [
    (0xE82E91B1, 0xB32700D0), (0x44DE24C6, 0x88DFF51D), (0x3BEB65BC, 0x29514038),
    (0xC89C5933, 0xB328CBDF), (0x8D1EC211, 0xD405B4D5), (0x314EF5D5, 0x26E55F3C),
    (0xE92491B1, 0x607801C6), (0xF0A5B9D5, 0x82B2C1FB), (0x06064CF3, 0xFA51A3DA),
    (0xDA79FA60, 0x207CC464), (0xBED20560, 0x3C3C74F3), (0x89F9C9F8, 0xD2D14B61),
    (0x35FA38C6, 0xBB04A2C1), (0xCB49E3C6, 0xC5626167),
];
const CS64_FACTOR: u32 = 0x82893933;
const CS64_ADJUSTMENT0: u32 = 0xF2F90B1A;
const CS64_ADJUSTMENT1: u32 = 0x0B1A0000;

#[inline]
fn sigma0(x: u32) -> u32 { x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3) }
#[inline]
fn sigma1(x: u32) -> u32 { x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10) }

fn expand_schedule(w: &mut [u32; 64], first: usize) {
    for i in first..64 {
        w[i] = sigma1(w[i - 2])
            .wrapping_add(w[i - 7])
            .wrapping_add(sigma0(w[i - 15]))
            .wrapping_add(w[i - 16]);
    }
}

fn sha256_rounds(state: [u32; 8], w: &[u32; 64], first_round: usize) -> [u32; 8] {
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
    for i in first_round..64 {
        let k = if first_round == 14 && i == 14 { SHA256_ADJUSTMENT } else { SHA256_ROUND_CONSTANTS[i] };
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = g ^ (e & (f ^ g));
        let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(k).wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) | (c & (a | b));
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    [a, b, c, d, e, f, g, h]
}

fn token_sha256_init(head: &[u8]) -> [u32; 8] {
    let mut w = [0u32; 64];
    w[14] = ((head[0] as u32) << 16) | ((head[1] as u32) << 8) | (head[2] as u32);
    w[15] = u32::from_be_bytes([head[3], head[4], head[5], head[6]]);
    for i in 16..30 {
        let mut v = SHA256_SCHEDULE[i - 16];
        if i - 2 >= 14 {
            let s = sigma1(w[i - 2]);
            v = v.wrapping_add(if i == 16 { s ^ SHA256_SIGMA1_ADJUSTMENT } else { s });
        }
        if i - 7 >= 14 {
            v = v.wrapping_add(w[i - 7]);
        }
        if i - 15 >= 14 {
            let s = sigma0(w[i - 15]);
            v = v.wrapping_add(if i == 29 { s ^ SHA256_SIGMA0_ADJUSTMENT } else { s });
        }
        w[i] = v;
    }
    w[30] = sigma1(w[28])
        .wrapping_add(w[23])
        .wrapping_add(sigma0(w[15]))
        .wrapping_add(w[14])
        .wrapping_add(SHA256_ADJUSTMENT)
        .wrapping_sub(SHA256_ROUND_CONSTANTS[14]);
    expand_schedule(&mut w, 31);
    let working = sha256_rounds(SHA256_STATE, &w, 14);
    let mut out = [0u32; 8];
    for i in 0..8 { out[i] = SHA256_INITIAL[i].wrapping_add(working[i]); }
    out
}

fn token_sha256(data: &[u8]) -> [u8; 32] {
    let total_len = 57 + data.len();
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(&data[7..]);
    padded.push(0x80);
    let pad_zeros = (55usize.wrapping_sub(total_len)) % 64;
    padded.resize(padded.len() + pad_zeros, 0);
    padded.extend_from_slice(&((total_len as u64) * 8).to_be_bytes());
    let mut state = token_sha256_init(&data[..7]);
    for block in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        expand_schedule(&mut w, 16);
        let working = sha256_rounds(state, &w, 0);
        for i in 0..8 { state[i] = state[i].wrapping_add(working[i]); }
    }
    let mut out = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn cs64_digest(data: &[u8], digest: &[u8; 32]) -> (u32, u32) {
    let key_a = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]) | 1;
    let key_b = u32::from_le_bytes([digest[4], digest[5], digest[6], digest[7]]) | 1;
    let factors = [key_a.wrapping_mul(CS64_FACTOR), key_b.wrapping_mul(CS64_FACTOR)];
    let mut terms = Vec::with_capacity(32);
    let mut offsets = Vec::with_capacity(32);
    for (i, &(coeff, off)) in CS64_COEFFICIENTS.iter().enumerate() {
        terms.push(coeff.wrapping_mul(factors[i % 2]));
        offsets.push(off);
    }
    let mut buf = Vec::with_capacity(1 + data.len() + 7);
    buf.push(0);
    buf.extend_from_slice(data);
    while buf.len() % 4 != 0 { buf.push(0); }
    let mut words: Vec<u32> = buf.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    if words.len() % 2 != 0 { words.push(0); }
    for (i, word) in words.into_iter().enumerate() {
        let mut term = word.wrapping_mul(if i % 2 == 0 { key_a } else { key_b });
        let mut off = word.rotate_left(16).wrapping_mul(if i % 2 == 0 { 0xEF0569FB } else { 0xC31713DB });
        if i == 0 {
            term = term.wrapping_add(CS64_ADJUSTMENT0.wrapping_mul(factors[0]));
            off = off.wrapping_add(CS64_ADJUSTMENT1);
        }
        terms.push(term);
        offsets.push(off);
    }
    let (mut swap_acc, mut swap_last, mut state, mut acc) = (0u32, 0u32, 0u32, 0u32);
    for i in (0..terms.len()).step_by(2) {
        let (x, y) = (terms[i], terms[i + 1]);
        let t = offsets[i].wrapping_add(x).rotate_left(16);
        let q = t.wrapping_mul(0x76F50F34);
        swap_acc = swap_acc.wrapping_add(q.rotate_left(16).wrapping_mul(0xC3EFEA97).wrapping_add(q));
        let t = offsets[i + 1].wrapping_add(y).rotate_left(16);
        let q = t.wrapping_mul(0x1881CE3C);
        swap_last = q.rotate_left(16).wrapping_mul(0x35BD1EC9).wrapping_add(q);
        swap_acc = swap_acc.wrapping_add(swap_last);

        state = state.wrapping_mul(key_a).wrapping_add(x);
        state = state.rotate_left(16).wrapping_mul(0xCF98B111);
        state = state.rotate_left(16).wrapping_mul(0x87085B9F);
        state = state.rotate_left(16).wrapping_mul(0x12CEB96D);
        state = state.rotate_left(16).wrapping_mul(0x257E1D83);
        acc = acc.wrapping_add(state);
        let t = state.wrapping_mul(key_b).wrapping_add(y);
        let t = t.rotate_left(16).wrapping_mul(0xA27416F5);
        let t = t.rotate_left(16).wrapping_mul(0xD38396FF);
        state = t.rotate_left(16).wrapping_mul(0x7C932B89).rotate_left(16).wrapping_mul(0xBFA49F69);
        acc = acc.wrapping_add(state);
    }
    (swap_last ^ state, swap_acc ^ acc)
}

pub fn generate_sec_ms_gec_v2(unix_seconds: f64) -> String {
    let mut ticks = unix_seconds + WIN_EPOCH as f64;
    ticks -= ticks.rem_euclid(300.0);
    let ticks_100ns = (ticks * 10_000_000.0).round() as u64;
    let payload = format!("{ticks_100ns}{TRUSTED_CLIENT_TOKEN}");
    let digest = token_sha256(payload.as_bytes());
    let (a, b) = cs64_digest(payload.as_bytes(), &digest);
    let mut cs_bytes = [0u8; 8];
    cs_bytes[..4].copy_from_slice(&a.to_le_bytes());
    cs_bytes[4..].copy_from_slice(&b.to_le_bytes());
    format!("{}-{}", hex::encode_upper(cs_bytes), hex::encode_upper(digest))
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn adjusted_unix() -> f64 {
    now_unix() as f64 + CLOCK_SKEW.load(Ordering::Relaxed) as f64
}

fn parse_http_date(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(s.trim()).ok().map(|dt| dt.timestamp())
}

// ---- TCP + Proxy + TLS connection ----------------------------------------------------------------

fn parse_proxy_addr(proxy: &str) -> Option<(&str, &str, u16)> {
    let (scheme, rest) = proxy.split_once("://")?;
    let hostport = rest.split('/').next()?.rsplit('@').next()?;
    let (host, port) = hostport.rsplit_once(':')?;
    Some((scheme, host, port.parse().ok()?))
}

async fn connect_via_http_proxy(proxy_host: &str, proxy_port: u16, target_host: &str, target_port: u16) -> Result<TcpStream> {
    let mut stream = TcpStream::connect((proxy_host, proxy_port)).await?;
    let req = format!("CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\nProxy-Connection: Keep-Alive\r\n\r\n");
    stream.write_all(req.as_bytes()).await?;
    let resp = read_http_head(&mut stream).await?;
    let status = resp.lines().next().unwrap_or("");
    if !status.contains(" 200") {
        return Err(anyhow!("代理拒绝连接 Edge 语音服务（{status}）"));
    }
    Ok(stream)
}

async fn connect_via_socks5(proxy_host: &str, proxy_port: u16, target_host: &str, target_port: u16) -> Result<TcpStream> {
    let mut stream = TcpStream::connect((proxy_host, proxy_port)).await?;
    stream.write_all(&[0x05, 0x01, 0x00]).await?;
    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp).await?;
    if resp != [0x05, 0x00] {
        return Err(anyhow!("SOCKS5 代理握手失败"));
    }
    let host_bytes = target_host.as_bytes();
    let mut req = Vec::with_capacity(7 + host_bytes.len());
    req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8]);
    req.extend_from_slice(host_bytes);
    req.extend_from_slice(&target_port.to_be_bytes());
    stream.write_all(&req).await?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(anyhow!("SOCKS5 代理连接 Edge 语音服务失败"));
    }
    match head[3] {
        1 => { let mut b = [0u8; 6]; stream.read_exact(&mut b).await?; }
        3 => { let mut n = [0u8; 1]; stream.read_exact(&mut n).await?; let mut b = vec![0u8; n[0] as usize + 2]; stream.read_exact(&mut b).await?; }
        4 => { let mut b = [0u8; 18]; stream.read_exact(&mut b).await?; }
        _ => return Err(anyhow!("SOCKS5 代理返回了未知地址类型")),
    }
    Ok(stream)
}

async fn open_tcp() -> Result<TcpStream> {
    let proxy = net::effective_proxy();
    if let Some((scheme, phost, pport)) = parse_proxy_addr(&proxy) {
        let res = if scheme.starts_with("socks") {
            connect_via_socks5(phost, pport, HOST, 443).await
        } else {
            connect_via_http_proxy(phost, pport, HOST, 443).await
        };
        if let Ok(s) = res { return Ok(s); }
        // Only fall back to direct when the proxy came from the OS (not an explicit SubCast proxy).
        if !net::proxy().is_empty() { return res; }
    }
    TcpStream::connect((HOST, 443)).await.map_err(|_| anyhow!("连不上 Edge 在线语音服务，请检查网络或代理"))
}

async fn read_http_head<S: AsyncRead + Unpin>(stream: &mut S) -> Result<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    while buf.len() < 16_384 {
        stream.read_exact(&mut byte).await?;
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&buf).into_owned());
        }
    }
    Err(anyhow!("Edge 语音服务响应头过长"))
}

// ---- Minimal RFC 6455 WebSocket framing ----------------------------------------------------------

async fn ws_send<S: AsyncWrite + Unpin>(stream: &mut S, opcode: u8, payload: &[u8]) -> Result<()> {
    let mask = *uuid::Uuid::new_v4().as_bytes();
    let mask_key = [mask[0], mask[1], mask[2], mask[3]];
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.push(0x80 | (opcode & 0x0F));
    let len = payload.len();
    if len < 126 {
        frame.push(0x80 | (len as u8));
    } else if len <= 65_535 {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }
    frame.extend_from_slice(&mask_key);
    for (i, b) in payload.iter().enumerate() {
        frame.push(b ^ mask_key[i & 3]);
    }
    stream.write_all(&frame).await?;
    Ok(())
}

enum WsMsg {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

async fn ws_recv<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<WsMsg> {
    let mut acc = Vec::new();
    let mut kind: Option<u8> = None;
    loop {
        let mut hdr = [0u8; 2];
        stream.read_exact(&mut hdr).await?;
        let fin = (hdr[0] & 0x80) != 0;
        let opcode = hdr[0] & 0x0F;
        let masked = (hdr[1] & 0x80) != 0;
        let mut len = (hdr[1] & 0x7F) as u64;
        if len == 126 {
            let mut b = [0u8; 2];
            stream.read_exact(&mut b).await?;
            len = u16::from_be_bytes(b) as u64;
        } else if len == 127 {
            let mut b = [0u8; 8];
            stream.read_exact(&mut b).await?;
            len = u64::from_be_bytes(b);
        }
        if len > 16 * 1024 * 1024 { return Err(anyhow!("Edge 语音帧过大")); }
        let mask_key = if masked {
            let mut m = [0u8; 4];
            stream.read_exact(&mut m).await?;
            Some(m)
        } else {
            None
        };
        let mut data = vec![0u8; len as usize];
        if len > 0 { stream.read_exact(&mut data).await?; }
        if let Some(m) = mask_key {
            for (i, b) in data.iter_mut().enumerate() { *b ^= m[i & 3]; }
        }
        match opcode {
            0x8 => return Ok(WsMsg::Close),
            0x9 => { let _ = ws_send(stream, 0xA, &data).await; continue; }
            0xA => continue,
            0x1 | 0x2 => {
                kind = Some(opcode);
                acc.extend_from_slice(&data);
                if fin { break; }
            }
            0x0 => {
                acc.extend_from_slice(&data);
                if fin { break; }
            }
            _ => continue,
        }
    }
    match kind {
        Some(0x1) => Ok(WsMsg::Text(String::from_utf8_lossy(&acc).into_owned())),
        Some(0x2) => Ok(WsMsg::Binary(acc)),
        _ => Ok(WsMsg::Close),
    }
}

// ---- Edge SSML & synthesis -----------------------------------------------------------------------

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\r' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

pub fn format_rate(rate: f64) -> String {
    let pct = ((rate.clamp(0.5, 2.0) - 1.0) * 100.0).round() as i32;
    if pct >= 0 { format!("+{pct}%") } else { format!("{pct}%") }
}

fn rfc1123_now() -> String {
    chrono::Utc::now().format("%a %b %d %Y %H:%M:%S GMT+0000 (Coordinated Universal Time)").to_string()
}

enum HandshakeErr {
    RetryAfterSkew,
    RateLimited,
    Other(anyhow::Error),
}

async fn synthesize_once(voice: &str, text: &str, rate: f64) -> std::result::Result<Vec<u8>, HandshakeErr> {
    let tcp = open_tcp().await.map_err(HandshakeErr::Other)?;
    let server = ServerName::try_from(HOST).map_err(|_| HandshakeErr::Other(anyhow!("无效主机名")))?;
    let mut tls = TlsConnector::from(TLS_CONFIG.clone())
        .connect(server, tcp)
        .await
        .map_err(|_| HandshakeErr::Other(anyhow!("连接 Edge 在线语音服务失败")))?;

    let conn_id = uuid::Uuid::new_v4().simple().to_string();
    let gec = generate_sec_ms_gec_v2(adjusted_unix());
    let muid = uuid::Uuid::new_v4().simple().to_string().to_uppercase();
    let ws_key = base64::engine::general_purpose::STANDARD.encode(uuid::Uuid::new_v4().as_bytes());
    let ua = format!(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{CHROMIUM_MAJOR_VERSION}.0.0.0 Safari/537.36 Edg/{CHROMIUM_MAJOR_VERSION}.0.0.0"
    );
    let req = format!(
        "GET {PATH_BASE}?TrustedClientToken={TRUSTED_CLIENT_TOKEN}&ConnectionId={conn_id}&Sec-MS-GEC={gec}&Sec-MS-GEC-Version={SEC_MS_GEC_VERSION} HTTP/1.1\r\n\
         Host: {HOST}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {ws_key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Pragma: no-cache\r\n\
         Cache-Control: no-cache\r\n\
         Origin: {ORIGIN}\r\n\
         User-Agent: {ua}\r\n\
         Accept-Encoding: gzip, deflate, br\r\n\
         Accept-Language: en-US,en;q=0.9\r\n\
         Cookie: muid={muid};\r\n\r\n"
    );
    tls.write_all(req.as_bytes()).await.map_err(|e| HandshakeErr::Other(anyhow!("发送 Edge 语音请求失败：{e}")))?;
    let head = read_http_head(&mut tls).await.map_err(HandshakeErr::Other)?;
    let status_line = head.lines().next().unwrap_or("");
    let status_code: u16 = status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    if status_code == 429 {
        return Err(HandshakeErr::RateLimited);
    }
    if status_code == 403 {
        if let Some(date) = head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("date")).map(|(_, v)| v)) {
            if let Some(server_ts) = parse_http_date(date) {
                let skew = server_ts - now_unix();
                CLOCK_SKEW.store(skew, Ordering::Relaxed);
                return Err(HandshakeErr::RetryAfterSkew);
            }
        }
        return Err(HandshakeErr::Other(anyhow!("Edge 在线语音暂时拒绝连接（HTTP 403），可稍后重试或切换其他引擎")));
    }
    if status_code != 101 {
        return Err(HandshakeErr::Other(anyhow!("Edge 在线语音握手失败（HTTP {status_code}）")));
    }

    let ts = rfc1123_now();
    let cfg = format!(
        "X-Timestamp:{ts}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n\
         {{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\"sentenceBoundaryEnabled\":\"false\",\"wordBoundaryEnabled\":\"false\"}},\"outputFormat\":\"{OUTPUT_FORMAT}\"}}}}}}}}\r\n"
    );
    ws_send(&mut tls, 0x1, cfg.as_bytes()).await.map_err(|e| HandshakeErr::Other(anyhow!("发送 Edge 配置失败：{e}")))?;

    let req_id = uuid::Uuid::new_v4().simple().to_string();
    let rate_str = format_rate(rate);
    let escaped = xml_escape(text);
    let ssml = format!(
        "X-RequestId:{req_id}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{ts}Z\r\nPath:ssml\r\n\r\n\
         <speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'>\
         <voice name='{voice}'><prosody pitch='+0Hz' rate='{rate_str}' volume='+0%'>{escaped}</prosody></voice></speak>"
    );
    ws_send(&mut tls, 0x1, ssml.as_bytes()).await.map_err(|e| HandshakeErr::Other(anyhow!("发送 Edge 文本失败：{e}")))?;

    let mut mp3 = Vec::new();
    loop {
        match ws_recv(&mut tls).await.map_err(|e| HandshakeErr::Other(anyhow!("接收 Edge 语音中断：{e}")))? {
            WsMsg::Binary(buf) => {
                if buf.len() >= 2 {
                    let hlen = u16::from_be_bytes([buf[0], buf[1]]) as usize;
                    if 2 + hlen <= buf.len() {
                        let hdr = String::from_utf8_lossy(&buf[2..2 + hlen]);
                        if hdr.contains("Path:audio") {
                            mp3.extend_from_slice(&buf[2 + hlen..]);
                        }
                    }
                }
            }
            WsMsg::Text(msg) => {
                if msg.contains("Path:turn.end") { break; }
            }
            WsMsg::Close => break,
        }
    }
    let _ = ws_send(&mut tls, 0x8, &[]).await;
    if mp3.is_empty() {
        return Err(HandshakeErr::Other(anyhow!("Edge 在线语音没有返回音频")));
    }
    Ok(mp3)
}

/// Synthesises text with Edge Read Aloud, returning MP3 bytes.
pub async fn synthesize(voice: &str, text: &str, rate: f64) -> Result<Vec<u8>> {
    let now = now_unix();
    let until = COOLDOWN_UNTIL.load(Ordering::Relaxed);
    if now < until {
        return Err(anyhow!("Edge 在线语音暂时限流，请稍后再试或切换其他引擎"));
    }
    let _guard = GATE.lock().await;
    let voice = if voice.trim().is_empty() { "zh-CN-YunxiNeural" } else { voice.trim() };
    let res = tokio::time::timeout(Duration::from_secs(45), async {
        match synthesize_once(voice, text, rate).await {
            Ok(b) => Ok(b),
            Err(HandshakeErr::RetryAfterSkew) => match synthesize_once(voice, text, rate).await {
                Ok(b) => Ok(b),
                Err(HandshakeErr::RateLimited) => {
                    COOLDOWN_UNTIL.store(now_unix() + COOLDOWN_SECS, Ordering::Relaxed);
                    Err(anyhow!("Edge 在线语音暂时限流，请稍后再试或切换其他引擎"))
                }
                Err(HandshakeErr::RetryAfterSkew) => Err(anyhow!("Edge 在线语音暂时拒绝连接（HTTP 403），可稍后重试或切换其他引擎")),
                Err(HandshakeErr::Other(e)) => Err(e),
            },
            Err(HandshakeErr::RateLimited) => {
                COOLDOWN_UNTIL.store(now_unix() + COOLDOWN_SECS, Ordering::Relaxed);
                Err(anyhow!("Edge 在线语音暂时限流，请稍后再试或切换其他引擎"))
            }
            Err(HandshakeErr::Other(e)) => Err(e),
        }
    }).await;
    match res {
        Ok(r) => r,
        Err(_) => Err(anyhow!("Edge 在线语音超时，请检查网络或代理")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sec_ms_gec_v2_matches_rany2_edge_tts() {
        assert_eq!(
            generate_sec_ms_gec_v2(0.0),
            "52BC7A17B50DFEC9-28C4394775C8751CD2F6859C030C06DB1489ACAB3B3A7651831FC26895177ED6"
        );
        assert_eq!(
            generate_sec_ms_gec_v2(1700000000.0),
            "2CD23A2CA0B2595A-F3C846C370E05F9CC83ACC5F995C5F5D87A4D0F14CE8936A0F902E64099E38A2"
        );
        assert_eq!(
            generate_sec_ms_gec_v2(1763500000.0),
            "48811C5FAE272B19-D47EF460E9312014CC9FBD24B8C6C8ACBF2BF0E492BBB4706EAC5C602A6F2305"
        );
    }

    #[test]
    fn rate_and_xml_escape() {
        assert_eq!(format_rate(1.0), "+0%");
        assert_eq!(format_rate(1.25), "+25%");
        assert_eq!(format_rate(0.8), "-20%");
        assert_eq!(xml_escape("a < b & \"c\" > 'd'"), "a &lt; b &amp; &quot;c&quot; &gt; &apos;d&apos;");
    }

    #[test]
    fn parses_rfc2822_date() {
        let ts = parse_http_date("Thu, 19 Nov 2026 08:40:00 GMT").unwrap();
        assert!(ts > 1_790_000_000);
    }

    #[tokio::test]
    #[ignore]
    async fn live_edge_tts_smoke() {
        let mp3 = synthesize("zh-CN-YunxiNeural", "你好，这是 SubCast 多引擎朗读测试。", 1.0).await.unwrap();
        assert!(mp3.len() > 2000, "mp3 too small: {}", mp3.len());
    }
}
