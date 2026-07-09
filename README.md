# LAN Mesh

局域网多跳 Mesh 通信软件。同一群组内的设备只要存在任意直连链路即可通信，消息可经中间设备多跳转发到达目标，不要求全网互相连通。支持群聊、单聊与文件传输，无需互联网与中心服务器。

## 功能特性

- **群组通信**：创建群组（作为中继 Relay）或加入群组（作为叶子 Leaf），支持局域网自动发现可用群组，也可手动输入 IP 与端口加入。
- **群聊与单聊**：群发消息通过多跳洪泛到达所有成员；单聊通过路径发现建立定向转发路径，仅沿该路径传递，不做全群广播。
- **文件传输**：大文件自动分片发送，支持断点续传（中断后只补传缺失分片），接收完成后用 SHA256 校验完整性。
- **成员状态**：实时展示成员上线/下线、在线状态，以及直连与多跳可达标记。
- **连接保活**：邻居间定时心跳，超时自动判定离线；Leaf 角色断线后指数退避自动重连。
- **多网卡选择**：创建或加入群组时可指定本机网卡，避免系统默认路由选错网络接口。
- **离线分发**：Windows 端可打包为 NSIS 单文件安装程序，体积经 LTO 与符号裁剪优化。

> Android 客户端尚在规划中，当前仅提供 Windows 客户端。

## 前置条件

- **操作系统**：Windows 10/11（客户端基于 Tauri，依赖系统 WebView2，Win10/11 通常已预装）。
- **Rust 工具链**：使用 `edition = "2024"`，需较新版本的 stable Rust（含 MSVC 目标）。
- **Tauri CLI 2.x**：通过 `cargo install tauri-cli --version "^2"` 安装，或使用项目脚本调用。
- **PowerShell**：打包脚本基于 PowerShell（Windows 自带）。

## 快速开始

```bash
# 克隆后进入客户端目录，开发模式运行（会自动准备前端产物并启动窗口）
cd tauri-app
cargo tauri dev
```

启动后在界面中：选择网卡 → 创建群组（或发现并加入已有群组）→ 即可开始群聊/单聊与文件传输。

## 构建与生产运行

```bash
cd tauri-app

# 标准构建（生成 NSIS 安装包到 src-tauri/target/release/bundle/nsis/）
cargo tauri build

# 瘦身发布构建（opt-level=z、LTO、strip、panic=abort，产物更小）
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dist.ps1
```

前端为原生 HTML + JavaScript，无打包步骤：`scripts/prepare-dist.ps1` 仅将 `index.html` 与 `src/main.js` 复制到 `dist/` 供 Tauri 嵌入。

## 测试

核心库包含会话隔离、消息去重、路径发现等单元测试：

```bash
cargo test -p core
```

## 项目结构

```
lan_mesh/
├── Cargo.toml              # workspace 容器（声明 core / core-ffi / tauri-app）
├── core/                   # 核心通信库（网络 / 协议 / 路由 / 文件传输，不含 UI）
│   └── src/                # protocol.rs / frame.rs / session.rs / file_transfer.rs
├── core-ffi/               # Android FFI 适配层（占位，尚未实现）
├── tauri-app/              # Windows 客户端
│   ├── index.html          # 前端入口
│   ├── src/main.js         # 前端逻辑（原生 JS，无框架）
│   ├── scripts/            # 打包 / 发布脚本（PowerShell）
│   └── src-tauri/          # Tauri 后端（Rust，转调 core 接口）
└── ROADMAP.md              # 任务分解与进度
```

## 技术栈

| 层 | 技术 | 版本 |
|---|---|---|
| 核心库语言 | Rust（edition 2024） | — |
| 异步运行时 | tokio（features = full） | 1.52.3 |
| 序列化 | serde + serde_json（JSON，内部标签区分消息类型） | 1.0.228 / 1.0.150 |
| 唯一标识 | uuid（v4） | 1.23.4 |
| 文件分片编码 | base64（内嵌 JSON） | 0.22.1 |
| 完整性校验 | sha2（SHA256） | 0.10.9 |
| 传输层 | TCP（4 字节大端长度前缀 + JSON 体）/ UDP（设备发现广播） | — |
| Windows 客户端 | Tauri | 2 |
| 前端 | 原生 HTML + JavaScript（无框架、无打包器） | — |
| 安装包 | NSIS | — |
