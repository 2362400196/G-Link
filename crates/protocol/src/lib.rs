//! 客户端与中转节点之间的 UDP 隧道协议（v2：加密）。
//!
//! 包结构: header(12, 明文) | nonce(12) | AEAD 密文(载荷 + 16B tag)
//! header: magic(2) | version(1) | type(1) | session(4, LE) | seq(2, LE) | payload_len(2, LE)
//! payload_len 字段 = nonce(12) + 密文总长（含 tag）。
//!
//! 加密: ChaCha20-Poly1305，AAD = header 12 字节（防篡改 kind/session/seq）。
//! 密钥: SHA256("glink/v2" || token || session BE)，每个会话独立。
//! nonce: dir(1) || counter(8, LE) || 0x0000 00 —— dir 1=客户端→中转，2=中转→客户端；
//! counter 双方各自全局递增，且密钥按会话隔离，保证 (key, nonce) 永不重用。

pub const MAGIC: u16 = 0x5047;
pub const VERSION: u8 = 2;
pub const HEADER_LEN: usize = 12;

/// 建立会话，密文载荷 = token + 目标地址
pub const TYPE_OPEN: u8 = 1;
/// 内层数据
pub const TYPE_DATA: u8 = 2;
/// 保活
pub const TYPE_KEEPALIVE: u8 = 3;
/// 关闭会话
pub const TYPE_CLOSE: u8 = 4;

/// nonce 方向标识
pub const DIR_CLIENT: u8 = 1;
pub const DIR_RELAY: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: u8,
    pub session: u32,
    pub seq: u16,
}

/// 解析一个 UDP 包的明文 header，返回 (header, 密文段 nonce+ct)。
pub fn parse(buf: &[u8]) -> Option<(Header, &[u8])> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    let magic = u16::from_le_bytes([buf[0], buf[1]]);
    if magic != MAGIC || buf[2] != VERSION {
        return None;
    }
    let kind = buf[3];
    let session = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let seq = u16::from_le_bytes([buf[8], buf[9]]);
    let payload_len = u16::from_le_bytes([buf[10], buf[11]]) as usize;
    if buf.len() < HEADER_LEN + payload_len {
        return None;
    }
    Some((
        Header { kind, session, seq },
        &buf[HEADER_LEN..HEADER_LEN + payload_len],
    ))
}

/// 写入包头（明文部分），密文由调用方（encrypt）随后追加。
pub fn write_header(buf: &mut Vec<u8>, kind: u8, session: u32, seq: u16, payload_len: usize) {
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.push(VERSION);
    buf.push(kind);
    buf.extend_from_slice(&session.to_le_bytes());
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(&(payload_len as u16).to_le_bytes());
}

/// OPEN 明文载荷: token_len(1) + token + target_len(1) + target
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    pub token: String,
    pub target: String,
}

pub fn encode_open(token: &str, target: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(2 + token.len() + target.len());
    buf.push(token.len() as u8);
    buf.extend_from_slice(token.as_bytes());
    buf.push(target.len() as u8);
    buf.extend_from_slice(target.as_bytes());
    buf
}

pub fn parse_open(payload: &[u8]) -> Option<Open> {
    let token_len = *payload.first()? as usize;
    if payload.len() < 1 + token_len + 1 {
        return None;
    }
    let token = std::str::from_utf8(&payload[1..1 + token_len]).ok()?.to_string();
    let rest = &payload[1 + token_len..];
    let target_len = *rest.first()? as usize;
    if rest.len() < 1 + target_len {
        return None;
    }
    let target = std::str::from_utf8(&rest[1..1 + target_len]).ok()?.to_string();
    Some(Open { token, target })
}

// ---------- 加密 ----------

pub mod crypto {
    use chacha20poly1305::{
        aead::{Aead, KeyInit, Payload},
        ChaCha20Poly1305, Key, Nonce,
    };
    use sha2::{Digest, Sha256};

    pub const NONCE_LEN: usize = 12;
    pub const TAG_LEN: usize = 16;

    /// 会话密钥：同一会话内所有包（OPEN/DATA/KEEPALIVE/CLOSE）共用
    pub fn session_key(token: &str, session: u32) -> Key {
        let mut h = Sha256::new();
        h.update(b"glink/v2");
        h.update(token.as_bytes());
        h.update(session.to_be_bytes());
        let digest: [u8; 32] = h.finalize().into();
        digest.into()
    }

    /// 加密并追加 nonce，返回可直接跟在 header 后的 wire 段。
    /// counter 取自调用方的全局递增计数器（1 起）。
    pub fn encrypt(key: &Key, header: &[u8], plaintext: &[u8], dir: u8, counter: u64) -> Vec<u8> {
        let cipher = ChaCha20Poly1305::new(key);
        let mut nonce = [0u8; NONCE_LEN];
        nonce[0] = dir;
        nonce[1..9].copy_from_slice(&counter.to_le_bytes());
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad: header })
            .expect("chacha20poly1305 encrypt");
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    /// 解密 wire 段（nonce+密文），AAD 为 header；认证失败返回 None。
    pub fn decrypt(key: &Key, header: &[u8], wire: &[u8]) -> Option<Vec<u8>> {
        if wire.len() < NONCE_LEN + TAG_LEN {
            return None;
        }
        let cipher = ChaCha20Poly1305::new(key);
        cipher
            .decrypt(Nonce::from_slice(&wire[..NONCE_LEN]), Payload {
                msg: &wire[NONCE_LEN..],
                aad: header,
            })
            .ok()
    }
}
