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
/// 链路信息回报（中转 → 客户端），载荷 = leg2_ms(u32 LE)，
/// 即中转节点 ↔ 游戏服务器的往返延迟；用于客户端估算游戏内端到端延迟
pub const TYPE_INFO: u8 = 5;

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

/// 单字段上限 255 字节（长度用 1 字节存）。超长在 UTF-8 字符边界截断。
fn clamp255(s: &str) -> &[u8] {
    let b = s.as_bytes();
    let mut end = b.len().min(255);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &b[..end]
}

pub fn encode_open(token: &str, target: &str) -> Vec<u8> {
    let tok = clamp255(token);
    let tgt = clamp255(target);
    let mut buf = Vec::with_capacity(2 + tok.len() + tgt.len());
    buf.push(tok.len() as u8);
    buf.extend_from_slice(tok);
    buf.push(tgt.len() as u8);
    buf.extend_from_slice(tgt);
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

    /// 从 nonce 提取全局计数器（第 0 字节是方向，1..9 是 counter LE）
    pub fn counter_from_nonce(nonce: &[u8]) -> Option<u64> {
        if nonce.len() < NONCE_LEN {
            return None;
        }
        Some(u64::from_le_bytes(nonce[1..9].try_into().unwrap()))
    }

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

/// 重放防护：按 (会话, 方向) 维护一个 64 计数器的滑动窗口。
/// 计数器高于历史最高的直接接受；窗口内的未见过计数器接受；重复或过旧（低于窗口下沿）拒绝。
///
/// UDP 允许乱序，窗口吸收正常重排；游戏链路上乱序幅度远小于 64，
/// 误杀风险可忽略，而重放包全部被挡下。
#[derive(Debug, Default)]
pub struct ReplayGuard {
    /// 已见过的最高计数器
    highest: u64,
    /// 位图：bit i = 计数器 (highest - i) 是否已见（bit 0 = highest）
    seen: u64,
}

const WINDOW: u32 = 64;

impl ReplayGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 返回 true = 首次出现（放行），false = 重放（丢弃）。
    pub fn check(&mut self, counter: u64) -> bool {
        if counter > self.highest {
            let shift = (counter - self.highest) as u32;
            self.seen = if shift >= WINDOW { 0 } else { self.seen << shift };
            self.seen |= 1; // bit 0 = 当前计数器
            self.highest = counter;
            return true;
        }
        let d = self.highest - counter;
        if d >= WINDOW as u64 {
            return false; // 落在窗口外，无法区分新旧，一律拒绝
        }
        let bit = 1u64 << d;
        if self.seen & bit != 0 {
            return false;
        }
        self.seen |= bit;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crypto::{counter_from_nonce, decrypt, encrypt, session_key};

    #[test]
    fn header_roundtrip() {
        let mut buf = Vec::new();
        write_header(&mut buf, TYPE_DATA, 0x1234_5678, 0xABCD, 0);
        assert_eq!(buf.len(), HEADER_LEN);
        let (hdr, rest) = parse(&buf).unwrap();
        assert_eq!(hdr.kind, TYPE_DATA);
        assert_eq!(hdr.session, 0x1234_5678);
        assert_eq!(hdr.seq, 0xABCD);
        assert_eq!(rest.len(), 0);
        // payload_len 超过实际数据 → 解析失败
        let mut bad = buf.clone();
        bad[10] = 0xFF;
        bad[11] = 0xFF;
        assert!(parse(&bad).is_none());
        // magic/版本不符 → 拒绝
        let mut bad = buf.clone();
        bad[0] = 0;
        assert!(parse(&bad).is_none());
        let mut bad = buf;
        bad[2] = 99;
        assert!(parse(&bad).is_none());
    }

    #[test]
    fn open_roundtrip_and_clamp() {
        let enc = encode_open("tok-中文-token", "1.2.3.4:27015");
        let open = parse_open(&enc).unwrap();
        assert_eq!(open.token, "tok-中文-token");
        assert_eq!(open.target, "1.2.3.4:27015");

        // 超过 255 字节 → 在字符边界截断，仍可解析
        let long = "汉".repeat(300);
        let enc = encode_open(&long, "echo");
        let open = parse_open(&enc).unwrap();
        assert!(open.token.len() <= 255 * 4);
        assert_eq!(open.target, "echo");
        assert!(long.starts_with(&open.token));
        assert!(open.token.chars().count() <= 255);

        // 截断后的载荷必须合法
        let enc = encode_open(&"a".repeat(300), &"b".repeat(300));
        let open = parse_open(&enc).unwrap();
        assert_eq!(open.token, "a".repeat(255));
        assert_eq!(open.target, "b".repeat(255));
    }

    #[test]
    fn crypto_roundtrip_and_tamper() {
        let key = session_key("secret", 42);
        let header = [0u8; HEADER_LEN];
        let ct = encrypt(&key, &header, b"hello world", DIR_CLIENT, 7);
        assert_eq!(decrypt(&key, &header, &ct).unwrap(), b"hello world");

        // 密文翻转 → 认证失败
        let mut bad = ct.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0x01;
        assert!(decrypt(&key, &header, &bad).is_none());

        // AAD（header）翻转 → 认证失败
        let mut hdr2 = header;
        hdr2[3] = TYPE_CLOSE;
        assert!(decrypt(&key, &hdr2, &ct).is_none());

        // 错误密钥 → 失败
        assert!(decrypt(&session_key("other", 42), &header, &ct).is_none());

        // 计数器不同 → nonce 不同（同会话不会重用 (key,nonce)）
        let ct2 = encrypt(&key, &header, b"hello world", DIR_CLIENT, 8);
        assert_ne!(ct[..crypto::NONCE_LEN], ct2[..crypto::NONCE_LEN]);

        // counter_from_nonce 与方向字节
        assert_eq!(counter_from_nonce(&ct).unwrap(), 7);
        assert_eq!(ct[0], DIR_CLIENT);
    }

    #[test]
    fn replay_guard_basics() {
        let mut g = ReplayGuard::new();
        assert!(g.check(1));
        assert!(!g.check(1)); // 重放
        assert!(g.check(2));
        assert!(!g.check(2));
    }

    #[test]
    fn replay_guard_reorder_within_window() {
        let mut g = ReplayGuard::new();
        assert!(g.check(1));
        assert!(g.check(40));  // 窗口推进到 [1..40]（最低到 0）
        assert!(g.check(20));  // 窗口内未见过的乱序包 → 接受（UDP 正常重排）
        assert!(!g.check(20)); // 重复 → 拒绝
        assert!(g.check(30));  // 另一个未见过的窗口内包 → 接受
        assert!(!g.check(30));
        assert!(g.check(100)); // 窗口推进到 [37..100]
        assert!(g.check(60));  // 窗口内
        assert!(!g.check(20)); // 已滑出窗口 → 拒绝
        assert!(g.check(37));  // 恰在窗口下沿
        assert!(!g.check(36)); // 窗口外
    }

    #[test]
    fn replay_guard_far_old_rejected() {
        let mut g = ReplayGuard::new();
        assert!(g.check(1));
        assert!(g.check(1000));
        // 窗口 [937..1000]，远落后的一律拒绝
        assert!(!g.check(1));
        assert!(!g.check(900));
        assert!(!g.check(936));
        assert!(g.check(937));
    }

    #[test]
    fn session_keys_differ() {
        let a = session_key("t", 1);
        let b = session_key("t", 2);
        let c = session_key("u", 1);
        assert_ne!(a, b);
        assert_ne!(a, c);
    }
}
