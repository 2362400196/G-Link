//! PUBG 加速器客户端引擎（P1，"UU 模式"）：
//! 1. 按进程名发现游戏 PID，轮询 UDP 连接表得到游戏占用的本地端口；
//! 2. WinDivert 在网络层截获"源端口属于游戏"的出站 UDP，其余流量原样放行；
//! 3. 被截获的包经隧道（token 认证 UDP 协议）发往中转节点，由其转发到游戏服务器；
//! 4. 游戏服务器的回包经隧道返回后，伪造"从游戏服务器发来"的入站包注入本机，游戏无感知。
//!
//! 全程不注入、不 Hook、不改游戏内存（反作弊安全）。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::Parser;
use protocol::crypto;
use protocol::ReplayGuard;
use protocol::{
    encode_open, parse, write_header, DIR_CLIENT, DIR_RELAY, TYPE_DATA, TYPE_INFO, TYPE_KEEPALIVE,
    TYPE_OPEN,
};
use windivert::address::WinDivertAddress;
use windivert::prelude::*;
use windivert::WinDivert;
use windivert_sys::ChecksumFlags;

/// 隧道协议包上限
const MAX_PACKET: usize = 2048;

#[derive(Parser)]
struct Args {
    /// 中转节点地址 ip:port
    #[arg(long)]
    relay: String,
    /// 接入令牌（也可用环境变量 GLINK_TOKEN）
    #[arg(long, env = "GLINK_TOKEN")]
    token: String,
    /// 要加速的进程名（可多次指定）
    #[arg(long = "process", default_value = "TslGame.exe")]
    processes: Vec<String>,
    /// 进程与端口表刷新间隔（秒）
    #[arg(long, default_value_t = 2)]
    rescan_secs: u64,
    /// 调试：查询 UDP 端口归属后退出
    #[arg(long)]
    check_port: Option<u16>,
}

// ---------- WinDivert 句柄跨线程共享（底层 HANDLE 的 recv/send 均线程安全） ----------

struct DivertSync(WinDivert<NetworkLayer>);
unsafe impl Send for DivertSync {}
unsafe impl Sync for DivertSync {}

// ---------- 会话簿 ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FlowKey {
    local_port: u16,
    dst_ip: Ipv4Addr,
    dst_port: u16,
}

impl Hash for FlowKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.local_port.hash(state);
        self.dst_ip.hash(state);
        self.dst_port.hash(state);
    }
}

struct SessionRec {
    /// 游戏包源地址（本机接口 IP），回包注入的目标
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    dst_port: u16,
    local_port: u16,
    /// 捕获时的接口索引，入站注入必须复用，否则包会被协议栈丢弃
    if_idx: u32,
    sub_if_idx: u32,
    next_seq: u16,
    last_seen: Instant,
}

#[derive(Default)]
struct SessionBook {
    by_key: HashMap<FlowKey, (u32, SessionRec)>,
    by_id: HashMap<u32, FlowKey>,
    next_id: u32,
}

struct Shared {
    divert: DivertSync,
    /// 已连接中转的隧道 socket
    tunnel: UdpSocket,
    token: String,
    /// 目标进程名列表
    processes: Vec<String>,
    /// 目标进程 PID 集合（后台定期刷新 + 按需单点补充）
    game_pids: RwLock<HashSet<u32>>,
    /// 端口归属缓存: port -> (checked_at, is_game)
    /// 命中即免系统调用；未命中才实时查 UDP 连接表（应对短命进程/新端口）
    attrib: Mutex<HashMap<u16, (Instant, bool)>>,
    book: Mutex<SessionBook>,
    stats_tunneled: AtomicU64,
    stats_reinjected: AtomicU64,
    stats_passthrough: AtomicU64,
    stats_dropped: AtomicU64,
    /// 是否已打印过"流量已接入"横幅
    banner_shown: AtomicBool,
    /// 延迟探针会话（0 = 未建立；回包不注入，用于测隧道 RTT）
    probe_session: AtomicU32,
    /// leg2：中转 ↔ 游戏服务器 RTT（毫秒，0 = 未知；来自中转 TYPE_INFO 回报）
    leg2_ms: AtomicU32,
    /// 发往中转方向的 nonce 计数器（全局递增）
    nonce_counter: AtomicU64,
    /// 中转→客户端方向的重放窗口（按会话）
    replay: Mutex<HashMap<u32, ReplayGuard>>,
}

impl Shared {
    /// 取会话；新流则登记并返回 true（调用方需补发 OPEN）
    fn get_or_create(&self, key: FlowKey, src_ip: Ipv4Addr, if_idx: u32, sub_if_idx: u32) -> (u32, bool) {
        let mut book = self.book.lock().unwrap();
        if let Some((id, rec)) = book.by_key.get_mut(&key) {
            rec.src_ip = src_ip;
            rec.last_seen = Instant::now();
            return (*id, false);
        }
        book.next_id = book.next_id.wrapping_add(1);
        let id = book.next_id ^ (nanos() as u32);
        book.by_key.insert(
            key,
            (
                id,
                SessionRec {
                    src_ip,
                    dst_ip: key.dst_ip,
                    dst_port: key.dst_port,
                    local_port: key.local_port,
                    if_idx,
                    sub_if_idx,
                    next_seq: 0,
                    last_seen: Instant::now(),
                },
            ),
        );
        book.by_id.insert(id, key);
        (id, true)
    }

    fn bump_seq(&self, key: FlowKey) -> u16 {
        let mut book = self.book.lock().unwrap();
        if let Some((_, rec)) = book.by_key.get_mut(&key) {
            rec.next_seq = rec.next_seq.wrapping_add(1);
            rec.next_seq
        } else {
            0
        }
    }

    /// 中转→客户端方向的重放检查（按会话滑动窗口）
    fn check_replay(&self, session: u32, counter: u64) -> bool {
        self.replay
            .lock()
            .unwrap()
            .entry(session)
            .or_default()
            .check(counter)
    }

    /// 端口是否属于目标进程。先查缓存，未命中则实时查 UDP 连接表归属。
    /// 轮询永远追不上短命进程（如 DNS 查询），按包实时归属才是可靠方案。
    fn is_game_port(&self, port: u16) -> bool {
        const TTL_HIT: Duration = Duration::from_secs(30);
        const TTL_MISS: Duration = Duration::from_millis(500);
        let now = Instant::now();
        {
            let cache = self.attrib.lock().unwrap();
            if let Some((ts, ok)) = cache.get(&port) {
                let ttl = if *ok { TTL_HIT } else { TTL_MISS };
                if now.duration_since(*ts) < ttl {
                    return *ok;
                }
            }
        }
        // 未命中/过期：查一次连接表，并对属主 PID 做单点实时校验
        let ok = port_owner(port)
            .map(|pid| self.pid_matches_target(pid))
            .unwrap_or(false);
        let mut cache = self.attrib.lock().unwrap();
        if cache.len() > 4096 {
            cache.clear(); // 粗暴防膨胀，重启缓存即可
        }
        if ok && cache.get(&port).map(|(_, was)| !*was).unwrap_or(true) {
            println!("接管端口 :{port}（归属目标进程）");
        }
        cache.insert(port, (now, ok));
        ok
    }

    /// PID 是否属于目标进程：先查集合，未命中则按需刷新该 PID 的进程名（短命进程友好）
    fn pid_matches_target(&self, pid: u32) -> bool {
        if self.game_pids.read().unwrap().contains(&pid) {
            return true;
        }
        let mut sys = sysinfo::System::new();
        let sip = sysinfo::Pid::from_u32(pid);
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sip]), true);
        let hit = sys
            .process(sip)
            .map(|p| {
                let name = p.name().to_string_lossy().to_lowercase();
                self.processes.iter().any(|t| t.to_lowercase() == name)
            })
            .unwrap_or(false);
        if hit {
            self.game_pids.write().unwrap().insert(pid);
        }
        hit
    }
}

fn nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

// ---------- 进程与端口发现 ----------

fn refresh_pids(sys: &mut sysinfo::System, targets: &[String]) -> HashSet<u32> {
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let mut pids = HashSet::new();
    for (pid, proc) in sys.processes() {
        let name = proc.name().to_string_lossy().to_lowercase();
        if targets.iter().any(|t| t.to_lowercase() == name) {
            pids.insert(pid.as_u32());
        }
    }
    pids
}

/// 查询 UDP 连接表，返回占用该本地端口的 PID
fn port_owner(port: u16) -> Option<u32> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedUdpTable, MIB_UDPTABLE_OWNER_PID, UDP_TABLE_OWNER_PID,
    };
    use windows::Win32::Networking::WinSock::AF_INET;

    unsafe {
        let mut size: u32 = 0;
        let _ = GetExtendedUdpTable(None, &mut size, false, AF_INET.0 as u32, UDP_TABLE_OWNER_PID, 0);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let rc = GetExtendedUdpTable(
            Some(buf.as_mut_ptr().cast()),
            &mut size,
            false,
            AF_INET.0 as u32,
            UDP_TABLE_OWNER_PID,
            0,
        );
        if rc != 0 {
            return None;
        }
        let table = &*(buf.as_ptr() as *const MIB_UDPTABLE_OWNER_PID);
        // dwLocalPort: 低 16 位存网络字节序端口
        let want = u16::swap_bytes(port) as u32;
        let rows = std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
        rows.iter().find(|r| (r.dwLocalPort & 0xFFFF) == want).map(|r| r.dwOwningPid)
    }
}

// ---------- 数据面 ----------

/// 构造 IPv4+UDP 包（校验和交由 WinDivertHelperCalcChecksums 计算）
fn build_ip_udp(src: Ipv4Addr, sport: u16, dst: Ipv4Addr, dport: u16, payload: &[u8], id: u16) -> Vec<u8> {
    let total = 20 + 8 + payload.len();
    let mut v = Vec::with_capacity(total);
    v.extend_from_slice(&[0x45, 0x00]);
    v.extend_from_slice(&(total as u16).to_be_bytes());
    v.extend_from_slice(&id.to_be_bytes());
    v.extend_from_slice(&[0x40, 0x00]); // DF
    v.push(64);
    v.push(17); // UDP
    v.extend_from_slice(&[0, 0]); // IP checksum 占位
    v.extend_from_slice(&src.octets());
    v.extend_from_slice(&dst.octets());
    v.extend_from_slice(&sport.to_be_bytes());
    v.extend_from_slice(&dport.to_be_bytes());
    v.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    v.extend_from_slice(&[0, 0]); // UDP checksum 占位
    v.extend_from_slice(payload);
    v
}

fn send_tunnel(shared: &Shared, kind: u8, session: u32, seq: u16, payload: &[u8]) -> Result<()> {
    // v2 加密：header(明文) | nonce | AEAD(载荷)
    let key = crypto::session_key(&shared.token, session);
    let mut pkt = Vec::with_capacity(
        protocol::HEADER_LEN + crypto::NONCE_LEN + payload.len() + crypto::TAG_LEN,
    );
    write_header(
        &mut pkt,
        kind,
        session,
        seq,
        crypto::NONCE_LEN + payload.len() + crypto::TAG_LEN,
    );
    let c = shared.nonce_counter.fetch_add(1, Ordering::Relaxed) + 1;
    pkt.extend(crypto::encrypt(&key, &pkt[..protocol::HEADER_LEN], payload, DIR_CLIENT, c));
    shared.tunnel.send(&pkt)?;
    Ok(())
}

/// 捕获主循环：截流游戏 UDP → 隧道；其余原样放行
fn capture_loop(shared: Arc<Shared>) -> Result<()> {
    let mut buf = [0u8; 65535];
    loop {
        let (addr_out, raw) = {
            let pkt = match shared.divert.0.recv(Some(&mut buf)) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("recv error: {e}");
                    continue;
                }
            };
            (pkt.address.clone(), pkt.data.to_vec())
        };

        if raw.len() < 28 {
            continue; // 异常短包，丢弃
        }
        let ihl = (raw[0] & 0x0F) as usize * 4;
        if raw[0] >> 4 != 4 || ihl < 20 || raw.len() < ihl + 8 {
            passthrough(&shared, &addr_out, raw)?;
            continue;
        }
        // 非首分片无法定位端口，直接丢弃（避免半隧道污染连接）
        if u16::from_be_bytes([raw[6], raw[7]]) & 0x1FFF != 0 {
            shared.stats_dropped.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let src_ip = Ipv4Addr::new(raw[12], raw[13], raw[14], raw[15]);
        let dst_ip = Ipv4Addr::new(raw[16], raw[17], raw[18], raw[19]);
        let udp = &raw[ihl..];
        let src_port = u16::from_be_bytes([udp[0], udp[1]]);
        let dst_port = u16::from_be_bytes([udp[2], udp[3]]);

        let is_game = shared.is_game_port(src_port);
        if !is_game {
            passthrough(&shared, &addr_out, raw)?;
            continue;
        }

        let key = FlowKey { local_port: src_port, dst_ip, dst_port };
        let (session, is_new) = shared.get_or_create(
            key,
            src_ip,
            addr_out.interface_index(),
            addr_out.subinterface_index(),
        );
        if is_new {
            let target = format!("{dst_ip}:{dst_port}");
            let open = encode_open(&shared.token, &target);
            // seq 用随机值保证 OPEN 包内容唯一（链路有反重放过滤）
            let seq = (nanos() & 0xFFFF) as u16;
            send_tunnel(&shared, TYPE_OPEN, session, seq, &open)?;
            println!("tunnel session {session:#x} -> {target} (local udp :{src_port})");
        }
        let seq = shared.bump_seq(key);
        let payload = &udp[8..];
        send_tunnel(&shared, TYPE_DATA, session, seq, payload)?;
        shared.stats_tunneled.fetch_add(1, Ordering::Relaxed);
        if !shared.banner_shown.swap(true, Ordering::Relaxed) {
            println!("[加速成功] 游戏流量已接入隧道 -> 经韩国中转 {}:{}（后续每个游戏会话都会在此记录）", dst_ip, dst_port);
        }
    }
}

/// 非目标进程流量：原样放行（透传，不进隧道）
fn passthrough(shared: &Shared, addr_out: &WinDivertAddress<NetworkLayer>, raw: Vec<u8>) -> Result<()> {
    let pkt = WinDivertPacket {
        address: addr_out.clone(),
        data: Cow::Owned(raw),
    };
    shared.divert.0.send(&pkt)?;
    shared.stats_passthrough.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// 回包线程：隧道 DATA → 伪造入站包注入协议栈
fn reply_loop(shared: Arc<Shared>) -> Result<()> {
    // v2 包 = header + nonce + 载荷 + tag，比 v1 多 28 字节
    let mut rbuf = vec![0u8; MAX_PACKET + protocol::HEADER_LEN + crypto::NONCE_LEN + crypto::TAG_LEN];
    loop {
        let len = match shared.tunnel.recv(&mut rbuf) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("tunnel recv error: {e}");
                continue;
            }
        };
        let Some((hdr, wire)) = parse(&rbuf[..len]) else { continue };
        // v2：解密回包（失败 = 令牌不符或包损坏，丢弃）
        let key = crypto::session_key(&shared.token, hdr.session);
        let Some(payload) = crypto::decrypt(&key, &rbuf[..protocol::HEADER_LEN], wire) else {
            continue;
        };
        // 方向校验：中转方向的包 nonce 前缀必须是 DIR_RELAY，
        // 同时挡住把本机上行包原样回灌（跨方向重放）的包
        if wire.len() < crypto::NONCE_LEN || wire[0] != DIR_RELAY {
            continue;
        }
        let counter = crypto::counter_from_nonce(wire);

        // 延迟探针回包：不注入协议栈，仅计算 RTT
        let probe = shared.probe_session.load(Ordering::Relaxed);
        if probe != 0 && hdr.session == probe {
            if payload.len() >= 8 {
                let sent = u64::from_le_bytes(payload[..8].try_into().unwrap());
                let rtt = nanos().saturating_sub(sent);
                let ms = rtt / 1_000_000;
                println!("[latency] {ms}");
                // 游戏内端到端估算 = 隧道 RTT（本机↔中转）+ leg2（中转↔游戏服务器）
                let leg2 = shared.leg2_ms.load(Ordering::Relaxed);
                if leg2 > 0 {
                    println!("[estimate] {}", ms + leg2 as u64);
                }
            }
            continue;
        }

        // 中转链路信息回报：leg2 = 中转 ↔ 游戏服务器 RTT
        if hdr.kind == TYPE_INFO {
            if payload.len() >= 4 {
                let ms = u32::from_le_bytes(payload[..4].try_into().unwrap());
                if shared.leg2_ms.swap(ms, Ordering::Relaxed) != ms {
                    println!("[leg2] 中转↔游戏服务器 RTT ≈ {ms} ms（游戏内延迟 ≈ 节点延迟 + {ms}ms）");
                }
            }
            continue;
        }
        if hdr.kind != TYPE_DATA { continue; }

        // 重放防护：已见过的 nonce 计数器直接丢弃
        match counter {
            Some(c) if shared.check_replay(hdr.session, c) => {}
            _ => continue,
        }

        let key = { shared.book.lock().unwrap().by_id.get(&hdr.session).copied() };
        let Some(key) = key else { continue };
        let rec = {
            let mut book = shared.book.lock().unwrap();
            match book.by_key.get_mut(&key) {
                Some((_, rec)) => {
                    rec.last_seen = Instant::now();
                    (
                        rec.src_ip,
                        rec.dst_ip,
                        rec.dst_port,
                        rec.local_port,
                        rec.if_idx,
                        rec.sub_if_idx,
                    )
                }
                None => continue,
            }
        };
        let (src_ip, dst_ip, dst_port, local_port, if_idx, sub_if_idx) = rec;
        ip_id_add();
        let bytes = build_ip_udp(dst_ip, dst_port, src_ip, local_port, &payload, ip_id_get());
        let mut pkt = unsafe { <WinDivertPacket<'static, NetworkLayer>>::new(bytes) };
        pkt.address.set_outbound(false);
        pkt.address.set_impostor(true);
        pkt.address.set_interface_index(if_idx);
        pkt.address.set_subinterface_index(sub_if_idx);
        pkt.recalculate_checksums(ChecksumFlags::new())?;
        shared.divert.0.send(&pkt)?;
        shared.stats_reinjected.fetch_add(1, Ordering::Relaxed);
    }
}

static IP_ID: AtomicU64 = AtomicU64::new(1);
fn ip_id_add() { IP_ID.fetch_add(1, Ordering::Relaxed); }
fn ip_id_get() -> u16 { IP_ID.load(Ordering::Relaxed) as u16 }

/// 保活 + 过期会话清理（15s 保活；180s 无流量回收）
fn keepalive_loop(shared: Arc<Shared>) {
    loop {
        std::thread::sleep(Duration::from_secs(15));
        let (ids, expired_ids): (Vec<u32>, Vec<u32>) = {
            let mut book = shared.book.lock().unwrap();
            let now = Instant::now();
            let expired: Vec<FlowKey> = book
                .by_key
                .iter()
                .filter(|(_, (_, rec))| now.duration_since(rec.last_seen) > Duration::from_secs(180))
                .map(|(k, _)| *k)
                .collect();
            let mut expired_ids = Vec::new();
            for k in &expired {
                if let Some((id, _)) = book.by_key.remove(k) {
                    book.by_id.remove(&id);
                    expired_ids.push(id);
                }
            }
            (book.by_key.values().map(|(id, _)| *id).collect(), expired_ids)
        };
        for id in expired_ids {
            shared.replay.lock().unwrap().remove(&id);
        }
        for id in ids {
            // 载荷放时间戳：包内容唯一，规避链路反重放
            let mut payload = [0u8; 8];
            payload.copy_from_slice(&nanos().to_le_bytes());
            let seq = (nanos() & 0xFFFF) as u16;
            let _ = send_tunnel(&shared, TYPE_KEEPALIVE, id, seq, &payload);
        }
    }
}

/// 统计输出线程
fn stats_loop(shared: Arc<Shared>) {
    loop {
        std::thread::sleep(Duration::from_secs(5));
        println!(
            "[stats] tunneled {} / reinjected {} / passthrough {} / dropped {} / sessions {}",
            shared.stats_tunneled.load(Ordering::Relaxed),
            shared.stats_reinjected.load(Ordering::Relaxed),
            shared.stats_passthrough.load(Ordering::Relaxed),
            shared.stats_dropped.load(Ordering::Relaxed),
            shared.book.lock().unwrap().by_key.len(),
        );
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let relay: SocketAddr = args.relay.parse().context("relay 格式应为 ip:port")?;

    // 调试分支：端口归属查询
    if let Some(port) = args.check_port {
        println!("port_owner({port}) = {:?}", port_owner(port));
        return Ok(());
    }

    // 初始进程发现
    let mut sys = sysinfo::System::new();
    let pids = refresh_pids(&mut sys, &args.processes);
    if pids.is_empty() {
        println!("目标进程未运行（{}），等待其启动...", args.processes.join(", "));
    } else {
        println!("已定位目标进程 PID: {:?}", pids);
    }

    let tunnel = UdpSocket::bind("0.0.0.0:0")?;
    tunnel.connect(relay)?;
    println!("隧道已连接中转节点 {relay}");

    // WinDivert 网络层：截获出站 IPv4 UDP（排除回环——反作弊/游戏本地通信不走隧道）
    let divert = match WinDivert::network(
        "outbound and udp and ip and ip.DstAddr != 127.0.0.1",
        0,
        WinDivertFlags::new(),
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("WinDivert 打开失败: {e}");
            eprintln!("提示: 请以管理员身份运行，且确保 WinDivert.dll / WinDivert64.sys 与 exe 同目录");
            bail!("WinDivert init failed");
        }
    };
    // 队列滞留上限 200ms，过旧的包直接丢弃，保证游戏低延迟
    let _ = divert.set_param(WinDivertParam::QueueTime, 200);
    let _ = divert.set_param(WinDivertParam::QueueLength, 16384);

    let mut book = SessionBook::default();
    book.next_id = (nanos() & 0x00FFFFFF) as u32;
    let shared = Arc::new(Shared {
        divert: DivertSync(divert),
        tunnel,
        token: args.token.clone(),
        processes: args.processes.clone(),
        game_pids: RwLock::new(pids.clone()),
        attrib: Mutex::new(HashMap::new()),
        book: Mutex::new(book),
        stats_tunneled: AtomicU64::new(0),
        stats_reinjected: AtomicU64::new(0),
        stats_passthrough: AtomicU64::new(0),
        stats_dropped: AtomicU64::new(0),
        banner_shown: AtomicBool::new(false),
        probe_session: AtomicU32::new(0),
        leg2_ms: AtomicU32::new(0),
        nonce_counter: AtomicU64::new(0),
        replay: Mutex::new(HashMap::new()),
    });

    // 后台线程：进程刷新、回包注入、保活、统计
    {
        let shared = shared.clone();
        let processes = args.processes.clone();
        let secs = args.rescan_secs.max(1);
        std::thread::spawn(move || {
            let mut had_game = !shared.game_pids.read().unwrap().is_empty();
            loop {
                std::thread::sleep(Duration::from_secs(secs));
                let pids = refresh_pids(&mut sysinfo::System::new(), &processes);
                let has_game = !pids.is_empty();
                if has_game && !had_game {
                    println!("[检测] 已发现目标进程 {}（PID: {:?}）", processes.join(", "), pids);
                } else if !has_game && had_game {
                    println!("[检测] 目标进程已退出，等待其启动...");
                }
                had_game = has_game;
                *shared.game_pids.write().unwrap() = pids;
            }
        });
    }
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            if let Err(e) = reply_loop(shared) {
                eprintln!("reply loop exited: {e}");
            }
        });
    }
    {
        let shared = shared.clone();
        std::thread::spawn(move || keepalive_loop(shared));
    }
    {
        let shared = shared.clone();
        std::thread::spawn(move || stats_loop(shared));
    }
    {
        // 延迟探针：独立会话目标为 "echo"，每 2s 发一次时间戳载荷
        let shared = shared.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            let sid = (nanos() as u32) | 1;
            shared.probe_session.store(sid, Ordering::Relaxed);
            let open = encode_open(&shared.token, "echo");
            let seq = (nanos() & 0xFFFF) as u16;
            let _ = send_tunnel(&shared, TYPE_OPEN, sid, seq, &open);
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let mut payload = [0u8; 8];
                payload.copy_from_slice(&nanos().to_le_bytes());
                let seq = (nanos() & 0xFFFF) as u16;
                let _ = send_tunnel(&shared, TYPE_DATA, sid, seq, &payload);
            }
        });
    }

    println!("加速引擎已启动（Ctrl+C 退出）");
    capture_loop(shared)
}
