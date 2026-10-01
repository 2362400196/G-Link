//! PUBG 加速器中转节点（P0）：UDP 会话中转 + 内置回显（echo），用于链路验证。
//!
//! 数据面: 客户端 OPEN 指定目标（"echo" 或 ip:port），后续 DATA 原样中转；
//! 目标的回包通过会话送回客户端最后所在的地址。
//!
//! 安全加固:
//! - 每会话 64 计数器滑动窗口重放防护（客户端→中转方向），nonce 方向字节校验
//! - 目标仅接受 IPv4 字面量（不做 DNS，避免阻塞事件循环），可用 --allow-target 限制网段
//! - 解密失败/认证失败告警限频，防伪造包刷屏
//! - token 支持 --token 参数或 GLINK_TOKEN 环境变量（推荐 systemd EnvironmentFile）
//!
//! 链路感知: OPEN 到真实目标时，后台探测中转 ↔ 目标的 ICMP RTT（缓存 60s），
//! 经 TYPE_INFO 回报客户端，用于估算游戏内端到端延迟。

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Parser;
use protocol::crypto;
use protocol::ReplayGuard;
use protocol::{
    parse, parse_open, write_header, DIR_CLIENT, DIR_RELAY, TYPE_CLOSE, TYPE_DATA, TYPE_INFO,
    TYPE_KEEPALIVE, TYPE_OPEN, HEADER_LEN,
};
use tokio::net::UdpSocket;

/// 游戏内层包很小，2KB 足够覆盖
const MAX_PACKET: usize = 2048;

/// leg2（中转 ↔ 目标）RTT 缓存时长
const LEG2_TTL: Duration = Duration::from_secs(60);

#[derive(Parser)]
struct Args {
    /// 监听地址
    #[arg(long, default_value = "0.0.0.0:41000")]
    bind: String,
    /// 接入令牌，防止中转被滥用（也可用环境变量 GLINK_TOKEN）
    #[arg(long, env = "GLINK_TOKEN")]
    token: String,
    /// 会话空闲超时（秒）
    #[arg(long, default_value_t = 60)]
    session_timeout: u64,
    /// 允许中转的目标网段（可多次指定，如 45.121.0.0/16；缺省不限制）。
    /// 限制后 OPEN 到白名单之外的目标会被拒绝，防止中转被当作开放 UDP 中继。
    #[arg(long = "allow-target")]
    allow_targets: Vec<String>,
}

/// IPv4 网段白名单规则
struct AllowRule {
    net: u32,
    mask: u32,
}

impl AllowRule {
    fn parse(s: &str) -> Result<Self> {
        let (ip_s, pfx) = match s.split_once('/') {
            Some((a, b)) => (a, b.parse::<u32>().context("前缀长度应为数字")?),
            None => (s, 32),
        };
        anyhow::ensure!(pfx <= 32, "前缀长度 0-32，得到 {pfx}");
        let ip: Ipv4Addr = ip_s.parse().with_context(|| format!("非法 IPv4: {ip_s}"))?;
        let mask = if pfx == 0 { 0 } else { u32::MAX << (32 - pfx) };
        Ok(Self { net: u32::from(ip) & mask, mask })
    }

    fn matches(&self, ip: Ipv4Addr) -> bool {
        u32::from(ip) & self.mask == self.net
    }
}

#[derive(Clone)]
enum Target {
    Echo,
    Udp(Arc<UdpSocket>),
}

struct Session {
    client: SocketAddr,
    target: Target,
    last_seen: Instant,
    /// 客户端→中转方向的重放窗口
    replay: ReplayGuard,
}

/// OPEN 尚未到达时缓存的早期数据包，OPEN 后按序回放
struct Pending {
    created: Instant,
    packets: Vec<(u16, Vec<u8>)>,
}

/// leg2 RTT 探测缓存 + 去重
#[derive(Default)]
struct Leg2 {
    map: Mutex<HashMap<Ipv4Addr, (Instant, u32)>>,
    inflight: Mutex<HashSet<Ipv4Addr>>,
}

struct Relay {
    sock: Arc<UdpSocket>,
    token: String,
    timeout: Duration,
    allow: Vec<AllowRule>,
    sessions: Mutex<HashMap<u32, Session>>,
    pending: Mutex<HashMap<u32, Pending>>,
    leg2: Leg2,
    /// 发往客户端方向的 nonce 计数器（全局递增）
    counter: AtomicU64,
}

// ---------- 告警限频（防伪造包刷屏） ----------

static LAST_WARN_MS: AtomicU64 = AtomicU64::new(0);
static SUPPRESSED: AtomicU64 = AtomicU64::new(0);

fn warn_limited(msg: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let last = LAST_WARN_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= 5000
        && LAST_WARN_MS
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        let sup = SUPPRESSED.swap(0, Ordering::Relaxed);
        if sup > 0 {
            println!("{msg}（近 5 秒已静默 {sup} 条同类告警）");
        } else {
            println!("{msg}");
        }
    } else {
        SUPPRESSED.fetch_add(1, Ordering::Relaxed);
    }
}

// ---------- leg2 探测 ----------

/// 从 ping 输出解析 RTT。
/// 不依赖任何本地化文本（中文 Windows 的输出是 GBK，"时间=" 经 UTF-8 转换即乱码）：
/// 找含 `ttl=`（大小写均可，中英文/Windows/Linux 输出均不翻译）的回复行，
/// RTT 数字永远紧跟在该行的 `ms` 之前（时间=41ms / time=41.2 ms / 时间<1ms）。
fn parse_ping_ms(s: &str) -> Option<u32> {
    for line in s.lines() {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("ttl=") {
            continue;
        }
        let ms_idx = lower.find("ms")?;
        let bytes = line.as_bytes();
        let mut end = ms_idx;
        while end > 0 && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
            start -= 1;
        }
        if start == end {
            continue;
        }
        if let Ok(v) = line[start..end].parse::<f64>() {
            return Some(v.round() as u32);
        }
    }
    None
}

/// 对目标发一次 ICMP ping 测 RTT（阻塞，调用方放 spawn_blocking）
fn ping_rtt(ip: Ipv4Addr) -> Option<u32> {
    let out = if cfg!(target_os = "windows") {
        std::process::Command::new("ping")
            .args(["-n", "1", "-w", "2000"])
            .arg(ip.to_string())
            .output()
            .ok()?
    } else {
        std::process::Command::new("ping")
            .args(["-c", "1", "-W", "2"])
            .arg(ip.to_string())
            .output()
            .ok()?
    };
    parse_ping_ms(&String::from_utf8_lossy(&out.stdout))
}

impl Relay {
    /// 构造发往客户端的加密 DATA 包（v2）
    fn encrypt_data(&self, session: u32, seq: u16, payload: &[u8]) -> Vec<u8> {
        let key = crypto::session_key(&self.token, session);
        let mut buf = Vec::with_capacity(
            HEADER_LEN + crypto::NONCE_LEN + payload.len() + crypto::TAG_LEN,
        );
        write_header(
            &mut buf,
            TYPE_DATA,
            session,
            seq,
            crypto::NONCE_LEN + payload.len() + crypto::TAG_LEN,
        );
        let c = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        buf.extend(crypto::encrypt(&key, &buf[..HEADER_LEN], payload, DIR_RELAY, c));
        buf
    }

    /// 回报 leg2 RTT（TYPE_INFO），客户端据此估算游戏内端到端延迟
    async fn send_info(&self, session: u32, ms: u32) {
        let key = crypto::session_key(&self.token, session);
        let client = {
            self.sessions
                .lock()
                .unwrap()
                .get(&session)
                .map(|s| s.client)
        };
        let Some(client) = client else { return };
        let mut buf = Vec::with_capacity(HEADER_LEN + crypto::NONCE_LEN + 4 + crypto::TAG_LEN);
        write_header(&mut buf, TYPE_INFO, session, 0, crypto::NONCE_LEN + 4 + crypto::TAG_LEN);
        let c = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        buf.extend(crypto::encrypt(&key, &buf[..HEADER_LEN], &ms.to_le_bytes(), DIR_RELAY, c));
        let _ = self.sock.send_to(&buf, client).await;
    }

    /// 探测/取缓存的中转 ↔ 目标 RTT，并回报给会话
    fn request_leg2(self: &Arc<Self>, ip: Ipv4Addr, session: u32) {
        {
            let map = self.leg2.map.lock().unwrap();
            if let Some((ts, ms)) = map.get(&ip) {
                if ts.elapsed() < LEG2_TTL {
                    let relay = self.clone();
                    let ms = *ms;
                    tokio::spawn(async move { relay.send_info(session, ms).await });
                    return;
                }
            }
        }
        // 同一目标同时只跑一个探测
        if !self.leg2.inflight.lock().unwrap().insert(ip) {
            return;
        }
        let relay = self.clone();
        tokio::spawn(async move {
            let ms = tokio::task::spawn_blocking(move || ping_rtt(ip))
                .await
                .ok()
                .flatten();
            relay.leg2.inflight.lock().unwrap().remove(&ip);
            match ms {
                Some(ms) => {
                    relay.leg2.map.lock().unwrap().insert(ip, (Instant::now(), ms));
                    relay.send_info(session, ms).await;
                }
                None => warn_limited(&format!("leg2 probe failed: {ip}（ICMP 不通或不响应，无法估算游戏内延迟）")),
            }
        });
    }

    async fn handle(self: &Arc<Self>, buf: &[u8], from: SocketAddr) {
        let Some((hdr, wire)) = parse(buf) else {
            return;
        };
        // v2：所有包均为密文。解密失败 = 令牌不符或包被篡改，静默丢弃（等效认证）
        let key = crypto::session_key(&self.token, hdr.session);
        let Some(payload) = crypto::decrypt(&key, &buf[..HEADER_LEN], wire) else {
            warn_limited(&format!("decrypt failed from {from} (bad token or forged packet)"));
            return;
        };
        // 方向校验：客户端发来的包 nonce 前缀必须是 DIR_CLIENT，
        // 同时挡住把中转自己的下行包原样回灌（跨方向重放）的攻击
        if wire[0] != DIR_CLIENT {
            warn_limited(&format!("wrong direction byte from {from}"));
            return;
        }
        let counter = crypto::counter_from_nonce(wire);
        match hdr.kind {
            TYPE_OPEN => self.open(hdr.session, &payload, from).await,
            TYPE_DATA => self.data(hdr.session, hdr.seq, &payload, counter).await,
            TYPE_KEEPALIVE => {
                let mut map = self.sessions.lock().unwrap();
                if let Some(s) = map.get_mut(&hdr.session) {
                    if counter.is_some_and(|c| s.replay.check(c)) {
                        s.last_seen = Instant::now();
                    }
                }
            }
            TYPE_CLOSE => {
                let mut map = self.sessions.lock().unwrap();
                if map
                    .get_mut(&hdr.session)
                    .is_some_and(|s| counter.is_some_and(|c| s.replay.check(c)))
                    && map.remove(&hdr.session).is_some()
                {
                    println!("session {:#x} closed by {}", hdr.session, from);
                }
            }
            _ => {}
        }
    }

    async fn open(self: &Arc<Self>, session: u32, payload: &[u8], from: SocketAddr) {
        let Some(open) = parse_open(payload) else {
            return;
        };
        if open.token != self.token {
            warn_limited(&format!("auth failed from {from}"));
            return;
        }
        let target = if open.target == "echo" {
            Target::Echo
        } else {
            // 目标仅接受 IPv4 字面量：杜绝 DNS 解析阻塞事件循环
            let Ok(addr) = open.target.parse::<SocketAddr>() else {
                warn_limited(&format!("bad target '{}' from {from}（仅支持 IP:端口）", open.target));
                return;
            };
            let SocketAddr::V4(v4) = addr else {
                warn_limited(&format!("bad target '{}' from {from}（仅支持 IPv4）", open.target));
                return;
            };
            if !self.allow.is_empty() && !self.allow.iter().any(|r| r.matches(*v4.ip())) {
                warn_limited(&format!("target {} not in allowlist, rejected from {from}", v4.ip()));
                return;
            }
            match connect_target(&addr).await {
                Ok(s) => {
                    // 后台探测中转 ↔ 目标 RTT，回报客户端（缓存命中则立即回）
                    self.request_leg2(*v4.ip(), session);
                    Target::Udp(Arc::new(s))
                }
                Err(e) => {
                    println!("bad target {} from {from}: {e}", open.target);
                    return;
                }
            }
        };

        // 目标回包 → 通过会话送回客户端
        if let Target::Udp(up) = &target {
            let relay = self.clone();
            let up = up.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; MAX_PACKET];
                loop {
                    // 会话已被回收/替换则退出
                    if !relay.sessions.lock().unwrap().contains_key(&session) {
                        break;
                    }
                    match tokio::time::timeout(Duration::from_secs(5), up.recv(&mut buf)).await {
                        Ok(Ok(len)) => relay.send_data(session, &buf[..len]).await,
                        Ok(Err(_)) => break,
                        Err(_) => continue, // 超时后复查会话
                    }
                }
            });
        }

        let replaced = self
            .sessions
            .lock()
            .unwrap()
            .insert(
                session,
                Session {
                    client: from,
                    target,
                    last_seen: Instant::now(),
                    replay: ReplayGuard::new(),
                },
            )
            .is_some();
        println!(
            "session {session:#x} opened by {from} -> {}{}",
            open.target,
            if replaced { " (replaced)" } else { "" }
        );

        // 回放 OPEN 之前到达的早期数据包
        let early = self.pending.lock().unwrap().remove(&session);
        if let Some(p) = early {
            for (seq, payload) in p.packets {
                self.data(session, seq, &payload, None).await;
            }
        }
    }

    async fn data(&self, session: u32, seq: u16, payload: &[u8], counter: Option<u64>) {
        let found = {
            let mut map = self.sessions.lock().unwrap();
            match map.get_mut(&session) {
                Some(s) => {
                    // 重放防护：已见过的 nonce 计数器直接丢弃
                    if counter.is_none_or(|c| !s.replay.check(c)) {
                        None
                    } else {
                        s.last_seen = Instant::now();
                        Some((s.client, s.target.clone()))
                    }
                }
                None => None,
            }
        };
        let (client, target) = match found {
            Some(v) => v,
            None => {
                // OPEN 可能还在路上，缓存早期数据包
                let mut pending = self.pending.lock().unwrap();
                if pending.len() < 1024 {
                    let entry = pending
                        .entry(session)
                        .or_insert_with(|| Pending { created: Instant::now(), packets: Vec::new() });
                    if entry.packets.len() < 32 {
                        entry.packets.push((seq, payload.to_vec()));
                    }
                }
                return;
            }
        };
        match target {
            Target::Echo => {
                // 回显真实 seq：既保证客户端按序匹配，也保证每个回包内容唯一
                let buf = self.encrypt_data(session, seq, payload);
                let _ = self.sock.send_to(&buf, client).await;
            }
            Target::Udp(up) => {
                let _ = up.send(payload).await;
            }
        }
    }

    /// 把目标的回包加密封装成 DATA 送回客户端
    async fn send_data(&self, session: u32, payload: &[u8]) {
        let client = {
            self.sessions
                .lock()
                .unwrap()
                .get(&session)
                .map(|s| s.client)
        };
        if let Some(client) = client {
            let buf = self.encrypt_data(session, 0, payload);
            let _ = self.sock.send_to(&buf, client).await;
        }
    }
}

/// 建立到游戏服务器的"已连接"UDP socket（内核只收该对端的回包）。
/// 传入的一定是 IP 字面量，不会触发 DNS。
async fn connect_target(addr: &SocketAddr) -> std::io::Result<UdpSocket> {
    let s = UdpSocket::bind("0.0.0.0:0").await?;
    s.connect(*addr).await?;
    Ok(s)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let allow = args
        .allow_targets
        .iter()
        .map(|s| AllowRule::parse(s))
        .collect::<Result<Vec<_>>>()?;
    let sock = Arc::new(
        UdpSocket::bind(args.bind.as_str())
            .await
            .with_context(|| format!("bind {}", args.bind))?,
    );
    println!(
        "pubg-relay listening on {} (token auth enabled, allow-target: {})",
        args.bind,
        if allow.is_empty() { "unrestricted".into() } else { format!("{} 网段", allow.len()) }
    );

    let relay = Arc::new(Relay {
        sock: sock.clone(),
        token: args.token,
        timeout: Duration::from_secs(args.session_timeout),
        allow,
        sessions: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        leg2: Leg2::default(),
        counter: AtomicU64::new(0),
    });

    // 空闲会话回收
    {
        let relay = relay.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(10));
            loop {
                tick.tick().await;
                let now = Instant::now();
                let mut map = relay.sessions.lock().unwrap();
                let before = map.len();
                map.retain(|_, s| now.duration_since(s.last_seen) < relay.timeout);
                let removed = before - map.len();
                drop(map);
                relay
                    .pending
                    .lock()
                    .unwrap()
                    .retain(|_, p| now.duration_since(p.created) < Duration::from_secs(10));
                if removed > 0 {
                    println!("reaped {removed} idle sessions");
                }
            }
        });
    }

    let mut buf = vec![0u8; MAX_PACKET + HEADER_LEN];
    loop {
        let (len, from) = sock.recv_from(&mut buf).await?;
        relay.handle(&buf[..len], from).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_rule_matches() {
        let r = AllowRule::parse("45.121.0.0/16").unwrap();
        assert!(r.matches("45.121.8.8".parse().unwrap()));
        assert!(!r.matches("45.122.0.1".parse().unwrap()));
        let r = AllowRule::parse("1.2.3.4").unwrap(); // 缺省 /32
        assert!(r.matches("1.2.3.4".parse().unwrap()));
        assert!(!r.matches("1.2.3.5".parse().unwrap()));
        assert!(AllowRule::parse("1.2.3.0/33").is_err());
        assert!(AllowRule::parse("foo/24").is_err());
    }

    #[test]
    fn ping_ms_parsing() {
        // 英文 Windows
        assert_eq!(
            parse_ping_ms("Reply from 223.5.5.5: bytes=32 time=12.4ms TTL=52"),
            Some(12)
        );
        // 中文 Windows（GBK 经 lossy 转换后 ASCII 字段完好）
        assert_eq!(
            parse_ping_ms("\u{fffd}\u{fffd}\u{fffd} 223.5.5.5 \u{fffd}\u{fffd}: \u{fffd}\u{fffd}=32 \u{fffd}\u{fffd}=41ms TTL=52"),
            Some(41)
        );
        // 正常 UTF-8 中文也应可用
        assert_eq!(parse_ping_ms("来自 223.5.5.5 的回复: 字节=32 时间=41ms TTL=52"), Some(41));
        // Linux（time 与 ms 之间有空格，ttl 小写）
        assert_eq!(
            parse_ping_ms("64 bytes from 223.5.5.5: icmp_seq=1 ttl=52 time=41.2 ms"),
            Some(41)
        );
        // <1ms
        assert_eq!(parse_ping_ms("时间<1ms TTL=52"), Some(1));
        // 丢包/不可达：无 TTL 行
        assert_eq!(parse_ping_ms("Request timed out.\r\n100% loss"), None);
        assert_eq!(parse_ping_ms("100% packet loss"), None);
    }
}
