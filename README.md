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
- **In-game latency estimate** — the relay actively measures its RTT to the game server and reports it back; the UI shows both "node latency" (you ↔ relay) and an estimated in-game latency (node latency + relay ↔ server)
- **Smart region select** — one click to switch to the lowest-latency node
- **Automatic failover** — if the active node keeps timing out (~10 s) while accelerating, it auto-switches to the best available node
- **Encrypted tunnel** — v2 protocol encrypts every packet with ChaCha20-Poly1305; token and traffic never travel in plaintext; replay protection included
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
# Round-trip latency and loss between you and the relay
accelctl --relay <node-ip>:41000 --token <token> probe

# Second hop: relay → game server RTT (in-game ping ≈ probe + this value)
accelctl --relay <node-ip>:41000 --token <token> probe --target <game-server-ip>:port
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

To upgrade later, just run the script again (omit `--token` to keep the configured token). The token is stored in `/opt/pubg-relay/token.env` (mode 600) and injected via systemd `EnvironmentFile`, so it never appears in the process list.

**Option 2: manual deployment** — download `bin/pubg-relay-linux`, then:

```bash
chmod +x pubg-relay-linux

# Run it (token is required to prevent relay abuse; prefer the env var so the
# token never shows up in the process list)
export GLINK_TOKEN=<your-token>
./pubg-relay-linux --bind 0.0.0.0:41000

# Optional: restrict which target ranges may be relayed (e.g. PUBG server ranges)
# to prevent the relay from being abused as an open UDP forwarder
# ./pubg-relay-linux --bind 0.0.0.0:41000 --allow-target 45.121.0.0/16 --allow-target 43.131.0.0/16

# systemd is recommended — example /etc/systemd/system/pubg-relay.service:
# [Service]
# EnvironmentFile=/opt/pubg-relay/token.env        # contains: GLINK_TOKEN=your-token (chmod 600)
# ExecStart=/opt/pubg-relay/pubg-relay-linux --bind 0.0.0.0:41000
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
| Access token | Must match the server's token (`--token` or `GLINK_TOKEN`) |

## FAQ

| Problem | Fix |
|---|---|
| No reaction on double-click / blocked by antivirus | Unsigned binary — choose "Run anyway" or whitelist it |
| WebView2Loader.dll not found | Make sure all package files stay in the same folder |
| Blank window | Install the WebView2 Runtime (link above) |
| Latency shows `--` | Node unreachable — check the server is running and UDP 41000 is open |
| "Node latency" differs from the in-game ping | They measure different things: the main number is the round trip to the relay (you ↔ relay); the in-game ping also includes the relay ↔ game-server hop. While accelerating, the UI shows an estimated "In-game ≈ X ms" measured via the relay — use that figure |
| Latency shows but game is not boosted | Make sure the process name matches the game (default `TslGame.exe`) |

## Technical Details

- Protocol (v2 encrypted): `header(12, plaintext) | nonce(12) | AEAD ciphertext(payload + 16B tag)`
  - Header: `magic(2) | ver(1) | type(1) | session(4) | seq(2) | payload_len(2)`
  - Types: OPEN (token + target authentication) / DATA / KEEPALIVE / CLOSE / INFO (relay reports leg-2 latency)
  - Encryption: ChaCha20-Poly1305 with per-session keys; the nonce carries a direction prefix + global counter (never reused); plaintext header is used as AAD
  - Replay protection: both sides keep a 64-counter sliding window per session; duplicate or stale counters are dropped; the nonce direction byte blocks cross-direction replay
- Latency estimation: on OPEN to a real target, the relay probes the target IP with one ICMP echo (cached 60 s) and reports the relay ↔ game-server RTT via an INFO packet; the client shows "in-game ≈ tunnel RTT + leg2". If the game server blocks ICMP, the estimate is hidden and only node latency is shown
- How acceleration works: WinDivert captures outbound UDP at the network layer (loopback 127.0.0.1 excluded) → the owning process is resolved via the OS UDP connection table → packets of the target process enter the tunnel, everything else passes through
- Safety: no DLL injection, no game memory access — compatible with BattlEye anti-cheat; the relay only accepts IPv4 literal targets (no DNS) with optional `--allow-target` CIDR allowlist; prefer the `GLINK_TOKEN` environment variable over command-line tokens

## License

For learning and research purposes only. Please comply with your local laws and regulations.
