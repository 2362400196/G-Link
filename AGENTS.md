# G-Link 项目协作约定

## 改动后的收尾流程（每次改动完成必须完整执行）

1. **编译产物**
   - `cargo build --release`（Windows 全部产物）
   - 若改动了 `relay-server` 或 `protocol`：`cargo build --release --target x86_64-unknown-linux-musl -p relay-server`
   - 编译前确认 `pubg-accel-gui.exe` 没在运行，否则链接器报 os error 32（文件被占用）
2. **更新产物目录**
   - 拷贝 `target/release/{accel-client,pubg-accel-gui,accelctl}.exe` 到 `dist/`
   - 拷贝 `target/x86_64-unknown-linux-musl/release/pubg-relay` 到 `dist/pubg-relay-linux` 和 `bin/pubg-relay-linux`（后者是 install.sh 的下载源，必须同步）
3. **重新打包**：用 `dist/` 下固定 8 个文件重建根目录 `G-Link-加速器.zip`（平铺结构，不套文件夹）：
   `accel-client.exe, accelctl.exe, pubg-accel-gui.exe, pubg-relay-linux, WebView2Loader.dll, WinDivert.dll, WinDivert64.sys, 使用说明.txt`
4. **提交并推送两个远程**（提交信息用中文）：
   - `git push github main`（GitHub）
   - `git push main main`（Gitee）
   - GitHub 直连超时时，用本机代理单次推送：`git -c http.proxy=socks5://127.0.0.1:10808 push github main`
5. 推送后确认两个远程都到达同一提交；`dist/`、`G-Link-加速器.zip` 本身不入库（已在 .gitignore）

## 其他约定

- 提交信息风格：中文一行标题 + 要点列表，参考 git log 既有风格
- 服务端二进制 `bin/pubg-relay-linux` 随仓库提交（install.sh 依赖），属例外不入 .gitignore
- 令牌等敏感信息不出现在命令行参数里（用 GLINK_TOKEN 环境变量）
