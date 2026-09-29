#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! PUBG 加速器 GUI：管理 accel-client 引擎子进程，实时展示日志与统计。
//! 启动时自动请求管理员权限（WinDivert 截流需要）。

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Emitter, Manager};

#[derive(Default)]
struct Engine {
    child: Mutex<Option<Child>>,
    running: AtomicBool,
    /// 引擎输出环形缓冲，前端轮询取走
    logs: Arc<Mutex<VecDeque<String>>>,
}

fn push_log(engine: &Engine, line: String) {
    push_log_by_arc(&engine.logs, line);
}

fn push_log_by_arc(q: &Arc<Mutex<VecDeque<String>>>, line: String) {
    let mut q = q.lock().unwrap();
    if q.len() > 500 {
        q.clear();
    }
    q.push_back(line);
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 写探针文件检测管理员权限（零依赖）
fn is_elevated() -> bool {
    let probe = std::path::Path::new(r"C:\Windows\.pubg_accel_admin_probe");
    match std::fs::write(probe, b"ok") {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

fn relaunch_elevated() {
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = Command::new("powershell")
        .args([
            "-NoProfile",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &format!("Start-Process -FilePath '{}' -Verb RunAs", exe.display()),
        ])
        .spawn();
}

/// 自动检测游戏进程（tasklist 扫描，零依赖）
#[tauri::command]
fn detect_game() -> Option<String> {
    let out = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("\"tslgame.exe\"") || lower.starts_with("\"pubg.exe\"") {
            return line.split(',').next().map(|p| p.trim_matches('"').to_string());
        }
    }
    None
}

fn kill_child(state: &Engine) {
    let mut guard = state.child.lock().unwrap();
    if let Some(mut child) = guard.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    state.running.store(false, Ordering::Relaxed);
}

#[tauri::command]
fn engine_running(state: tauri::State<Engine>) -> bool {
    state.running.load(Ordering::Relaxed)
}

#[tauri::command]
fn engine_logs(state: tauri::State<Engine>) -> Vec<String> {
    let mut q = state.logs.lock().unwrap();
    q.drain(..).collect()
}

#[tauri::command]
fn start_engine(
    app: tauri::AppHandle,
    state: tauri::State<Engine>,
    relay: String,
    token: String,
    process: String,
) -> Result<(), String> {
    if state.running.load(Ordering::Relaxed) {
        return Err("加速引擎已在运行".into());
    }
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("路径错误")?
        .join("accel-client.exe");
    if !exe.exists() {
        return Err(format!("未找到引擎程序: {}", exe.display()));
    }
    let mut child = Command::new(&exe)
        .args(["--relay", &relay, "--token", &token, "--process", &process])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("启动失败: {e}"))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    state.child.lock().unwrap().replace(child);
    state.running.store(true, Ordering::Relaxed);

    // stdout / stderr → 日志缓冲 + 前端事件（双通道）
    let logs_shared = Arc::clone(&app.state::<Engine>().logs);
    for stream in [
        Box::new(stdout) as Box<dyn std::io::Read + Send>,
        Box::new(stderr) as Box<dyn std::io::Read + Send>,
    ] {
        let app2 = app.clone();
        let logs2 = Arc::clone(&logs_shared);
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                push_log_by_arc(&logs2, line.clone());
                let _ = app2.emit("engine-log", line);
            }
        });
    }

    // 退出监视线程
    let app3 = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(400));
        let engine = app3.state::<Engine>();
        let mut guard = engine.child.lock().unwrap();
        match guard.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(status)) => {
                    *guard = None;
                    drop(guard);
                    engine.running.store(false, Ordering::Relaxed);
                    let code = status.code().unwrap_or(-1);
                    push_log(&engine, format!("[gui] 引擎进程退出（code {code}）"));
                    let _ = app3.emit("engine-exited", code);
                    break;
                }
                Ok(None) => {}
                Err(_) => {
                    engine.running.store(false, Ordering::Relaxed);
                    break;
                }
            },
            None => {
                engine.running.store(false, Ordering::Relaxed);
                break;
            }
        }
    });
    Ok(())
}

#[tauri::command]
fn stop_engine(state: tauri::State<Engine>) -> Result<(), String> {
    kill_child(&state);
    Ok(())
}

/// 探测节点延迟：发一个 OPEN(echo 目标) 包，等回包算 RTT。返回 None = 不通/超时。
#[tauri::command]
async fn probe_node(addr: String, token: String) -> Result<Option<f64>, String> {
    tauri::async_runtime::spawn_blocking(move || probe_once(&addr, &token))
        .await
        .map_err(|e| e.to_string())?
}

fn probe_once(addr: &str, token: &str) -> Result<Option<f64>, String> {
    use std::net::UdpSocket;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    let dest = if addr.contains(':') {
        addr.to_string()
    } else {
        format!("{addr}:41000")
    };
    sock.connect(&dest).map_err(|e| e.to_string())?;

    // 会话号：pid ^ 时间纳秒，避免与引擎/其他探测冲突
    let session = {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        std::process::id() ^ nanos.rotate_left(16)
    };

    // 1) OPEN(echo) 建立回显会话
    let open_payload = protocol::encode_open(token, "echo");
    let mut pkt = Vec::new();
    protocol::write_header(&mut pkt, protocol::TYPE_OPEN, session, 0, open_payload.len());
    pkt.extend_from_slice(&open_payload);
    sock.send(&pkt).map_err(|e| e.to_string())?;

    // 2) DATA(seq=0)，载荷含毫秒时间戳确保每个包内容唯一（规避链路反重放）
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut req = Vec::with_capacity(protocol::HEADER_LEN + 8);
    protocol::write_header(&mut req, protocol::TYPE_DATA, session, 0, 8);
    req.extend_from_slice(&ts.to_le_bytes());
    sock.send(&req).map_err(|e| e.to_string())?;

    // 3) 等 DATA(seq=0) 回包（relay 对 echo 会话原样回 DATA）
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_millis(2500);
    let mut buf = [0u8; 512];
    let mut got = false;
    while Instant::now() < deadline {
        let remain = deadline.saturating_duration_since(Instant::now());
        let _ = sock.set_read_timeout(Some(remain));
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Some((h, _)) = protocol::parse(&buf[..n]) {
                    if h.kind == protocol::TYPE_DATA && h.session == session && h.seq == 0 {
                        got = true;
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }

    // 4) CLOSE 清理会话（尽力而为）
    let mut close_pkt = Vec::new();
    protocol::write_header(&mut close_pkt, protocol::TYPE_CLOSE, session, 0, 0);
    let _ = sock.send(&close_pkt);

    if got {
        Ok(Some(t0.elapsed().as_secs_f64() * 1000.0))
    } else {
        Ok(None)
    }
}

fn main() {
    // WinDivert 需要管理员权限：未提权则通过 UAC 重启自身
    if std::env::var("PUBG_ACCEL_NO_ELEVATE").is_err() && !is_elevated() {
        relaunch_elevated();
        return;
    }
    tauri::Builder::default()
        .manage(Engine::default())
        .setup(|app| {
            // Windows 亚克力毛玻璃，失败则退回 Mica
            #[cfg(target_os = "windows")]
            {
                if let Some(window) = app.get_webview_window("main") {
                    if window_vibrancy::apply_acrylic(&window, Some((238, 243, 240, 225))).is_err() {
                        let _ = window_vibrancy::apply_mica(&window, None);
                    }
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![start_engine, stop_engine, engine_running, engine_logs, probe_node, detect_game])
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                kill_child(&window.app_handle().state::<Engine>());
            }
        })
        .run(tauri::generate_context!())
        .expect("tauri 启动失败");
}
