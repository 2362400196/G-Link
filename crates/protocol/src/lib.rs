//! 客户端与中转节点之间的 UDP 隧道协议（P0 最小实现）。
//!
//! 包结构: magic(2) | version(1) | type(1) | session(4, LE) | seq(2, LE) | payload_len(2, LE) | payload

pub const MAGIC: u16 = 0x5047;
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 12;

/// 建立会话，payload = token + 目标地址
pub const TYPE_OPEN: u8 = 1;
/// 内层数据
pub const TYPE_DATA: u8 = 2;
/// 保活
pub const TYPE_KEEPALIVE: u8 = 3;
/// 关闭会话
pub const TYPE_CLOSE: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: u8,
    pub session: u32,
    pub seq: u16,
}

/// 解析一个 UDP 包，返回 (header, payload)。
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

/// 写入包头，payload 由调用方随后追加。
pub fn write_header(buf: &mut Vec<u8>, kind: u8, session: u32, seq: u16, payload_len: usize) {
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.push(VERSION);
    buf.push(kind);
    buf.extend_from_slice(&session.to_le_bytes());
    buf.extend_from_slice(&seq.to_le_bytes());
    buf.extend_from_slice(&(payload_len as u16).to_le_bytes());
}

/// OPEN 载荷: token_len(1) + token + target_len(1) + target
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
