# G-Link 极速游戏加速器

G-Link 是一款使用 Rust 编写的轻量级游戏加速器，采用「按进程精准分流」方案：只截流目标游戏（如 PUBG）的 UDP 流量并经中转节点转发，不注入、不修改游戏内存，不影响其他程序的网络。

```
┌─────────┐   UDP 隧道(加密令牌)   ┌──────────────┐   转发    ┌────────────┐
│ 游戏进程 │ ←──────────────────→ │ 中转节点(海外) │ ←──────→ │ 游戏服务器  │
└─────────┘    WinDivert 截流     └──────────────┘           └────────────┘
```

## 功能特性

- **按进程分流**：只接管目标进程（默认 `TslGame.exe`）的 UDP 流量，下载、网页、语音不受影响
- **多节点管理**：支持添加多个中转节点，点击切换；节点延迟每 2.5 秒自动探测
- **实时监控**：延迟、丢包率、实时速率、隧道包/回注包统计一目了然
- **节点热切换**：加速过程中切换节点无需重启游戏
- **游戏库绑定**：点击游戏卡片自动绑定对应加速进程
- **原生性能**：客户端引擎纯 Rust + WinDivert，中转节点 tokio 异步高并发

## 项目结构

```
g-link/
├── crates/
│   ├── protocol/       # 隧道协议定义（v2 加密，客户端 ↔ 中转节点）
│   ├── relay-server/   # 中转节点服务端（部署在海外服务器）
│   ├── accel-client/   # 加速引擎（WinDivert 截流 + 隧道）
│   ├── accelctl/       # 链路探测调试工具
│   └── gui/            # 桌面客户端（Tauri 2 + 原生前端）
│       └── ui/         # 前端页面（自研组件，无 UI 框架）
├── bin/                # pubg-relay-linux 服务端预编译二进制（供一键脚本下载）
├── install.sh          # 服务端一键安装/升级脚本
├── game-booster-ui.html # 客户端 UI 设计稿
└── third_party/        # WinDivert 2.2.2 驱动（构建时需自行放置，不入库）
```

## 普通用户：使用分发包

1. 解压分发包（如 `G-Link-分发包.zip`）到任意目录，**保持所有文件在同一文件夹**
2. 双击 `pubg-accel-gui.exe`，UAC 提示点「是」（加速引擎需要管理员权限）
3. 在节点选择器中选择加速节点（默认已预置）
4. 点击「一键加速」，然后启动游戏进对局即可
5. 关闭窗口会自动停止加速

> 首次运行若提示缺少 WebView2，请安装微软官方离线包：https://go.microsoft.com/fwlink/p/?LinkId=2124703

## 开发者：从源码构建

环境要求：Windows 10+、Rust 1.75+、Node 不需要（前端静态资源直接打包）

```powershell
# 1. 放置 WinDivert 驱动
#    下载 WinDivert-2.2.2-A 解压到 third_party/，
#    .cargo/config.toml 已配置 WINDIVERT_PATH 指向 third_party/WinDivert-2.2.2-A/x64

# 2. 编译
cargo build --release

# 3. 产物
#    target/release/pubg-accel-gui.exe   桌面客户端
#    target/release/accel-client.exe     加速引擎（需与 WinDivert.dll / WinDivert64.sys 同目录）
#    target/release/accelctl.exe         链路探测工具
#    target/x86_64-unknown-linux-gnu/release/relay-server（Linux 交叉编译）
```

调试链路是否通畅：

```powershell
accelctl --relay <节点IP>:41000 --token <令牌> probe
```

## 服务端部署（中转节点）

**方式一：一键脚本（推荐）** —— 自动下载 `bin/pubg-relay-linux`、配置 systemd 常驻、开机自启、崩溃自动拉起：

```bash
curl -fsSL https://gitee.com/zhuxiaohuaqn/g-link/raw/main/install.sh -o install.sh
bash install.sh --token <你的令牌>
```

升级服务端时重复运行即可（不传 `--token` 则沿用已配置的令牌）。

**方式二：手动部署** —— 下载 `bin/pubg-relay-linux` 后：

```bash
chmod +x pubg-relay-linux

# 运行（token 必填，防止中转被滥用）
./pubg-relay-linux --bind 0.0.0.0:41000 --token <你的令牌>

# 推荐 systemd 常驻，示例 /etc/systemd/system/pubg-relay.service：
# [Service]
# ExecStart=/opt/pubg-relay/pubg-relay-linux --bind 0.0.0.0:41000 --token <你的令牌>
# Restart=always

systemctl daemon-reload && systemctl enable --now pubg-relay

# 防火墙放行 UDP 41000
ufw allow 41000/udp
```

## 客户端接入自建节点

界面左侧「设置」→「＋ 添加节点」：

| 字段 | 说明 |
|---|---|
| 节点名称 | 随意起名，含地区关键词可自动识别国旗（如"韩国·首尔"） |
| 节点地址 | `IP:端口`，默认端口 41000 |
| 国家图标 | 自动识别或手动指定 |
| 访问令牌 | 与服务端 `--token` 一致 |

## 常见问题

| 问题 | 解决 |
|---|---|
| 双击无反应 / 被杀毒拦截 | 未签名程序，选择「仍要运行」或加入白名单 |
| 找不到 WebView2Loader.dll | 确认分发包所有文件在同一文件夹 |
| 界面空白 | 安装 WebView2 Runtime（见上方链接） |
| 延迟显示 `--` | 节点不可达，检查服务端是否运行、UDP 41000 是否放行 |
| 延迟有数值但游戏没提速 | 确认加速进程名与游戏实际进程一致（默认 TslGame.exe） |

## 技术细节

- 协议：`magic(2) | ver(1) | type(1) | session(4) | seq(2) | payload_len(2) | payload`
  - OPEN（令牌+目标地址认证建会话）/ DATA / KEEPALIVE / CLOSE
- 加速原理：WinDivert 网络层截流 → 查询系统 UDP 连接表按端口归属进程判定 → 命中目标进程的包进隧道，其余放行
- 安全红线：不注入 DLL、不读写游戏内存，兼容 BattlEye 反作弊

## 许可

仅供学习研究使用，请遵守当地法律法规。
