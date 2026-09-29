//! P0 链路验证工具：经中转节点测量 UDP 往返延迟与丢包。
//!
//! 用法: accelctl --relay 1.2.3.4:41000 --token xxx probe

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use protocol::{encode_open, parse, write_header, TYPE_DATA, TYPE_OPEN};

#[derive(Parser)]
struct Args {
    /// 中转节点地址 ip:port
    #[arg(long)]
    relay: String,
    /// 接入令牌
    #[arg(long, default_value = "changeme")]
    token: String,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 经中转节点回显测量 RTT 与丢包
    Probe {
        /// 探测次数
        #[arg(long, default_value_t = 20)]
        count: u32,
        /// 每次间隔（毫秒）
        #[arg(long, default_value_t = 200)]
        interval_ms: u64,
        /// 载荷大小（字节）
        #[arg(long, default_value_t = 64)]
        size: usize,
    },
}

fn new_session() -> u32 {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    pid ^ nanos.rotate_left(16)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let Cmd::Probe {
        count,
        interval_ms,
        size,
    } = args.cmd;

    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    sock.connect(&args.relay).await?;
    println!("relay: {}", args.relay);

    let session = new_session();

    // 建立回显会话
    let open_payload = encode_open(&args.token, "echo");
    let mut pkt = Vec::new();
    write_header(&mut pkt, TYPE_OPEN, session, 0, open_payload.len());
    pkt.extend_from_slice(&open_payload);
    sock.send(&pkt).await?;

    let mut rtts = Vec::new();
    let mut lost = 0u32;

    for i in 0..count {
        // 载荷前 8 字节放毫秒时间戳，确保每个包内容唯一，排除链路反重放干扰
        let mut body = vec![0u8; size.max(8)];
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        body[..8].copy_from_slice(&ts.to_le_bytes());
        let mut req = Vec::with_capacity(protocol::HEADER_LEN + body.len());
        write_header(&mut req, TYPE_DATA, session, i as u16, body.len());
        req.extend_from_slice(&body);
        sock.send(&req).await?;

        let start = Instant::now();
        let mut got = false;
        let deadline = Duration::from_secs(3);
        while start.elapsed() < deadline {
            let mut rbuf = vec![0u8; 4096];
            let remain = deadline - start.elapsed();
            match tokio::time::timeout(remain, sock.recv(&mut rbuf)).await {
                Ok(Ok(len)) => {
                    if let Some((hdr, _)) = parse(&rbuf[..len]) {
                        if hdr.kind == TYPE_DATA && hdr.seq == i as u16 {
                            got = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }

        if got {
            let rtt = start.elapsed();
            println!("#{i:02}  {:>7.1} ms", rtt.as_secs_f64() * 1000.0);
            rtts.push(rtt);
        } else {
            lost += 1;
            println!("#{i:02}   timeout");
        }
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
    }

    // 关闭会话
    let mut pkt = Vec::new();
    write_header(&mut pkt, protocol::TYPE_CLOSE, session, 0, 0);
    let _ = sock.send(&pkt).await;

    println!("----");
    if rtts.is_empty() {
        bail!("all {count} probes lost");
    }
    let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
    let avg = rtts.iter().sum::<Duration>() / rtts.len() as u32;
    let min = rtts.iter().min().unwrap();
    let max = rtts.iter().max().unwrap();
    println!(
        "sent {count}  recv {}  loss {:.0}%  rtt min {:.1} / avg {:.1} / max {:.1} ms",
        rtts.len(),
        lost as f64 / count as f64 * 100.0,
        ms(min),
        ms(&avg),
        ms(max)
    );
    Ok(())
}
