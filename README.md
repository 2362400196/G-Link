English | [简体中文](README.zh-CN.md)

# G-Link Game Booster

G-Link is a lightweight game accelerator written in Rust. It uses a **per-process traffic splitting** approach: only the UDP traffic of the target game (e.g. PUBG) is intercepted and forwarded through a relay node. It never injects into or touches game memory, and it does not affect any other program's network.

```
┌───────────┐   UDP Tunnel (encrypted)   ┌──────────────┐  Forward   ┌───────────────┐
│ Game proc │ ◄───────────────────────► │ Relay (abroad)│ ◄────────► │ Game server   │
└───────────┘      WinDivert capture     └──────────────┘            └───────────────┘
```

![G-Link UI Preview](docs/ui-preview.png)

## Features

- **Per-process splitting** — only the target process's UDP traffic (default `TslGame.exe`) is captured; loopback traffic is excluded automatically; downloads, browsing and voice chat stay untouched
- **Multi-node management** — add multiple relay nodes and switch by click; latency is probed every 2.5 s
- **Smart region select** — one click to switch to the lowest-latency node
- **Automatic failover** — if the active node keeps timing out (~10 s) while accelerating, it auto-switches to the best available node
- **Encrypted tunnel** — v2 protocol encrypts every packet with ChaCha20-Poly1305; token and traffic never travel in plaintext
- **Real-time monitoring** — latency, packet loss, live throughput, and direct-connection latency comparison at a glance
- **Hot node switching** — switch nodes mid-game without restarting the game
- **Game library binding** — clicking a game card binds its process automatically
- **Native performance** — pure Rust client engine + WinDivert; tokio-based async relay

## Project Structure

```
g-link/
├── crates/
│   ├── protocol/       # Tunnel protocol (v2 encryption, client ↔ relay)
│   ├── relay-server/   # Relay node server (deployed on overseas servers)
│   ├── accel-client/   # Acceleration engine (WinDivert capture + tunnel)
│   ├── accelctl/       # Link probing / debugging tool
│   └── gui/            # Desktop client (Tauri 2 + vanilla frontend)
│       ├── ui/         # Frontend pages (index.html / app.js / style.css / assets)
│       └── icons/      # App icons
├── bin/                # Prebuilt pubg-relay-linux server binary (for install script)
└── install.sh          # One-click server install / upgrade script
```

> `third_party/` (WinDivert 2.2.2 driver) and `dist/` (release package output) are local-only directories and are not committed.

## For Users: Release Package

1. Extract the release package (e.g. `G-Link.zip`) to any folder — **keep all files in the same folder**
2. Double-click `pubg-accel-gui.exe` and click "Yes" on the UAC prompt (the engine needs admin rights)
3. Pick a relay node in the node selector (a default node is preconfigured)
4. Click "Accelerate", then start your game
5. Closing the window automatically stops acceleration

> If WebView2 is missing on first launch, install the official offline runtime: https://go.microsoft.com/fwlink/p/?LinkId=2124703

## For Developers: Build from Source

Requirements: Windows 10+, Rust 1.75+ (Node.js not needed — the frontend is bundled as static assets)

```powershell
# 1. Place the WinDivert driver
#    Download WinDivert-2.2.2-A and extract it into third_party/;
#    .cargo/config.toml already sets WINDIVERT_PATH to third_party/WinDivert-2.2.2-A/x64

# 2. Build
cargo build --release

# 3. Artifacts
#    target/release/pubg-accel-gui.exe                        Desktop client
#    target/release/accel-client.exe                          Engine (must sit next to WinDivert.dll / WinDivert64.sys)
#    target/release/accelctl.exe                              Link probing tool
#    target/release/pubg-relay.exe                            Relay server (Windows, for local debugging)
#    target/x86_64-unknown-linux-musl/release/pubg-relay      Relay server (Linux static cross-build, for deployment)
```

Verify the link works:

```powershell
accelctl --relay <node-ip>:41000 --token <token> probe
```

## Server Deployment (Relay Node)

**Option 1: one-click script (recommended)** — downloads `bin/pubg-relay-linux`, sets up systemd, enables auto-start on boot and auto-restart on crash:

```bash
# GitHub
curl -fsSL https://raw.githubusercontent.com/2362400196/G-Link/main/install.sh -o install.sh
# Gitee mirror (mainland China)
# curl -fsSL https://gitee.com/zhuxiaohuaqn/g-link/raw/main/install.sh -o install.sh

bash install.sh --token <your-token>
```

To upgrade later, just run the script again (omit `--token` to keep the configured token).

**Option 2: manual deployment** — download `bin/pubg-relay-linux`, then:

```bash
chmod +x pubg-relay-linux

# Run it (token is required to prevent relay abuse)
./pubg-relay-linux --bind 0.0.0.0:41000 --token <your-token>

# systemd is recommended — example /etc/systemd/system/pubg-relay.service:
# [Service]
# ExecStart=/opt/pubg-relay/pubg-relay-linux --bind 0.0.0.0:41000 --token <your-token>
# Restart=always

systemctl daemon-reload && systemctl enable --now pubg-relay

# Allow UDP 41000 in the firewall
ufw allow 41000/udp
```

## Connecting Your Own Node (Client)

In the app: sidebar "Settings" → "＋ Add node":

| Field | Description |
|---|---|
| Node name | Anything; region keywords auto-detect the flag icon (e.g. "Korea · Seoul") |
| Node address | `IP:port` (default port 41000) |
| Country icon | Auto-detected, or pick manually |
| Access token | Must match the server's `--token` |

## FAQ

| Problem | Fix |
|---|---|
| No reaction on double-click / blocked by antivirus | Unsigned binary — choose "Run anyway" or whitelist it |
| WebView2Loader.dll not found | Make sure all package files stay in the same folder |
| Blank window | Install the WebView2 Runtime (link above) |
| Latency shows `--` | Node unreachable — check the server is running and UDP 41000 is open |
| Latency shows but game is not boosted | Make sure the process name matches the game (default `TslGame.exe`) |

## Technical Details

- Protocol (v2 encrypted): `header(12, plaintext) | nonce(12) | AEAD ciphertext(payload + 16B tag)`
  - Header: `magic(2) | ver(1) | type(1) | session(4) | seq(2) | payload_len(2)`
  - Types: OPEN (token + target authentication) / DATA / KEEPALIVE / CLOSE
  - Encryption: ChaCha20-Poly1305 with per-session keys; the nonce carries a direction prefix + global counter (never reused); plaintext header is used as AAD
- How acceleration works: WinDivert captures outbound UDP at the network layer (loopback 127.0.0.1 excluded) → the owning process is resolved via the OS UDP connection table → packets of the target process enter the tunnel, everything else passes through
- Safety: no DLL injection, no game memory access — compatible with BattlEye anti-cheat

## License

For learning and research purposes only. Please comply with your local laws and regulations.
