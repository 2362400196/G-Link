//! PUBG 加速器中转节点（P0）：UDP 会话中转 + 内置回显（echo），用于链路验证。
//!
//! 数据面: 客户端 OPEN 指定目标（"echo" 或 ip:port），后续 DATA 原样中转；
//! 目标的回包通过会话送回客户端最后所在的地址。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use protocol::{
    parse, parse_open, write_header, TYPE_CLOSE, TYPE_DATA, TYPE_KEEPALIVE, TYPE_OPEN,
    HEADER_LEN,
};
use tokio::net::UdpSocket;

/// 游戏内层包很小，2KB 足够覆盖
const MAX_PACKET: usize = 2048;

/// 建立到游戏服务器的"已连接"UDP socket（内核只收该对端的回包）
async fn connect_target(addr: &str) -> std::io::Result<UdpSocket> {
    let s = UdpSocket::bind("0.0.0.0:0").await?;
    s.connect(addr).await?;
    Ok(s)
}

#[derive(Parser)]
struct Args {
    /// 监听地址
    #[arg(long, default_value = "0.0.0.0:41000")]
    bind: String,
    /// 接入令牌，防止中转被滥用
    #[arg(long)]
    token: String,
    /// 会话空闲超时（秒）
    #[arg(long, default_value_t = 60)]
    session_timeout: u64,
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
}

/// OPEN 尚未到达时缓存的早期数据包，OPEN 后按序回放
struct Pending {
    created: Instant,
    packets: Vec<(u16, Vec<u8>)>,
}

struct Relay {
    sock: Arc<UdpSocket>,
    token: String,
    timeout: Duration,
    sessions: Mutex<HashMap<u32, Session>>,
    pending: Mutex<HashMap<u32, Pending>>,
}

impl Relay {
    async fn handle(self: &Arc<Self>, buf: &[u8], from: SocketAddr) {
        let Some((hdr, payload)) = parse(buf) else {
            return;
        };
        match hdr.kind {
            TYPE_OPEN => self.open(hdr.session, payload, from).await,
            TYPE_DATA => self.data(hdr.session, hdr.seq, payload).await,
            TYPE_KEEPALIVE => {
                if let Some(s) = self.sessions.lock().unwrap().get_mut(&hdr.session) {
                    s.last_seen = Instant::now();
                }
            }
            TYPE_CLOSE => {
                if self.sessions.lock().unwrap().remove(&hdr.session).is_some() {
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
            println!("auth failed from {from}");
            return;
        }
        let target = if open.target == "echo" {
            Target::Echo
        } else {
            match connect_target(&open.target).await {
                Ok(s) => Target::Udp(Arc::new(s)),
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
                self.data(session, seq, &payload).await;
            }
        }
    }

    async fn data(&self, session: u32, seq: u16, payload: &[u8]) {
        let found = {
            let mut map = self.sessions.lock().unwrap();
            match map.get_mut(&session) {
                Some(s) => {
                    s.last_seen = Instant::now();
                    Some((s.client, s.target.clone()))
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
                let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
                write_header(&mut buf, TYPE_DATA, session, seq, payload.len());
                buf.extend_from_slice(payload);
                let _ = self.sock.send_to(&buf, client).await;
            }
            Target::Udp(up) => {
                let _ = up.send(payload).await;
            }
        }
    }

    /// 把目标的回包封装成 DATA 送回客户端
    async fn send_data(&self, session: u32, payload: &[u8]) {
        let client = {
            self.sessions
                .lock()
                .unwrap()
                .get(&session)
                .map(|s| s.client)
        };
        if let Some(client) = client {
            let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
            write_header(&mut buf, TYPE_DATA, session, 0, payload.len());
            buf.extend_from_slice(payload);
            let _ = self.sock.send_to(&buf, client).await;
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let sock = Arc::new(
        UdpSocket::bind(args.bind.as_str())
            .await
            .with_context(|| format!("bind {}", args.bind))?,
    );
    println!("pubg-relay listening on {} (token auth enabled)", args.bind);

    let relay = Arc::new(Relay {
        sock: sock.clone(),
        token: args.token,
        timeout: Duration::from_secs(args.session_timeout),
        sessions: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
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
