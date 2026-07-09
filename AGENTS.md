# AGENTS.md

本文件为 AI 编码代理(以及人类协作者)提供本项目的上下文说明、结构约定与操作规范。开始任何任务前请先阅读本文件。

## 项目概述

局域网 Mesh 通信软件。群组内允许任意数量设备加入,设备间只要存在任意直连链路即可,消息通过多跳转发到达目标(不要求全连通网状)。支持群发文本/文件消息与限定范围内的单聊。

- **Windows 端**:可作为 Relay(多连接中继)或 Leaf(单连接叶子)角色,使用 Tauri 框架。
- **Android 端**:固定为 Leaf 角色,仅维护单条连接,不承担多路转发/路由发现职责。
- **序列化**:统一使用 JSON(serde + serde_json),不使用 Protobuf 等二进制格式。
- **传输层**:TCP,应用层自行处理帧边界(长度前缀)。

## Workspace 结构

```
/                       # workspace 根,仅包含 Cargo.toml(workspace 容器)与本文件
├── core/               # 核心通信库(lib crate),网络/协议/路由/文件传输逻辑,不含任何UI代码
├── core-ffi/           # 供 Android JNI 绑定使用的 FFI 适配层,仅暴露 Leaf 角色能力
├── tauri-app/          # Windows 客户端(Tauri 壳 + 前端)
└── android-app/        # Android 客户端(Kotlin + Compose)
```

## 通用规则

1. **角色能力边界严格区分**。任何时候新增代码前,先确认该逻辑属于 Relay 专属、Leaf 专属,还是两者共用:
   - Relay 专属能力(多网卡绑定、监听、洪泛转发、路由发现中转)**不得**编译进 `core-ffi`(Android 侧禁止暴露)。
   - Leaf 专属能力(唯一连接、断线重连)在 `core` 中实现后由 `core-ffi` 选择性暴露。
2. **单个代码文件尽量不超过 1000 行**。超过时优先按职责拆分模块;确有特殊原因必须超过的,在文件头部注释说明原因与后续拆分条件。

## Git提交规范

### 格式

```
<emoji> <type>(<scope>): <subject>

<body>

<footer>
```

- **emoji**：视觉分类标识，必须使用
- **type**：`feat` / `fix` / `refactor` / `docs` / `test` / `chore` / `style` / `perf`
- **scope**：可选，如 `(opds)`、`(spider)`、`(api)`、`(web)`
- **subject**：中文标题，概括变更内容，首字无需空格
- **body**：英文或中英文混排，每行为一个 `- ` 开头的条目，描述具体变更
- **footer**：可选的 `Refs:` 或 `BREAKING CHANGE:`

### Emoji 对照表

| Type | Emoji | 含义 |
|---|---|---|
| `feat` | ✨ | 新功能 |
| `fix` | 🐛 | Bug 修复 |
| `refactor` | ♻️ | 代码重构 |
| `docs` | 📚 | 文档变更 |
| `test` | 🧪 | 测试相关 |
| `chore` | 🔧 | 工程化/依赖/配置 |
| `style` | 🎨 | 代码格式/样式 |
| `perf` | ⚡ | 性能优化 |
| `wip` | 🚧 | 进行中（仅临时使用，合并前必须 squash） |

### 示例

```
✨ feat(opds): 实现 OPDS 基础层——可见性控制与 EPUB 制品生命周期

- DB: add opds_visible, content_updated_at, epub_compiled_at columns
- Repository: add OPDS CRUD methods
- OpdsCompilationService: new cron-based scheduler

Refs: ROADMAP OPDS 书源服务构建与分发
```

```
🐛 fix(api): 修复定时更新策略变更后调度器未正确重载的并发问题
```

```
📚 docs: 添加 OPDS 书源服务任务到路线图
```

### 约定

- 多条变更在同一提交中时，`subject` 概括主要变更，`body` 逐条列举
- 每行 body 以 `- ` 开头，长度不超过 72 字符（英文）或适当截断
- **禁止**仅重复文件列表而无语义描述的提交
- **禁止**在提交消息中包含内部指令或占位符（如 "TODO"、"TBD"）


## 核心库(core)开发规范

- `core/src/lib.rs` 只作为 crate 入口,负责声明模块与 re-export 公开 API;具体实现按职责拆到独立模块文件中。当前约定:
  - `protocol.rs`: ID、消息结构、序列化辅助、时间戳辅助。
  - `frame.rs`: "4字节长度前缀 + JSON消息体"帧读写与帧错误。
  - `session.rs`: 会话、连接、路由、成员状态、Relay/Leaf 行为。
  - `session/tests.rs`: core 单元测试,可访问会话内部状态验证隔离与路由行为。
- 新增 core 代码时优先放入已有职责模块;只有出现稳定的新职责边界时才新增模块,避免把实现重新堆回 `lib.rs`。
- 异步运行时统一使用 `tokio`,禁止引入其他异步运行时(如 async-std)造成运行时冲突。
- 所有消息结构体通过 `serde` 派生 `Serialize`/`Deserialize`,消息类型区分使用 `#[serde(tag = "type")]` 内部标签,不使用外部标签或数字枚举。
- 网络字节序列使用"4字节长度前缀 + JSON消息体"的帧格式,长度前缀统一大端序,读写工具函数放在同一模块,不要在多处重复实现。
- 文件二进制分片在 JSON 消息中的编码方案以任务分解文档任务02中的选型结论为准,选定后全项目统一,不要出现两种编码方式混用。
- 去重缓存、路径缓存等状态,禁止使用全局静态变量(`static`/`lazy_static` 全局单例),必须挂载在会话实例(Session)结构体上,保证同一进程内多群组会话状态互相隔离(参考任务05"多会话并存验证")。
- 公开给 `core-ffi` 和 `tauri-app` 的接口保持精简且语义明确,内部实现细节(如具体的洪泛算法)不应通过公开接口暴露。

## Windows 客户端(tauri-app)开发规范

- Rust 后端通过本地路径依赖引用 `core` crate,禁止在 `tauri-app` 内重新实现任何网络/协议逻辑。
- 所有 Tauri command 只做参数校验与转调 `core` 接口,不写业务逻辑。
- 前端与后端之间的实时更新(新消息、成员上下线、传输进度)使用 Tauri 事件系统主动推送,禁止使用前端定时轮询后端状态。
- 前端技术栈无强制要求,但需保持轻量,不引入重型状态管理框架(需求规模不大)。

## Android 客户端(android-app)开发规范

- 通过 JNI 调用 `core-ffi` 暴露的接口,Kotlin 侧不重复实现网络协议逻辑。
- 长连接与消息收发必须运行在前台服务中,禁止依赖普通后台线程或 WorkManager 做长期连接保活。
- 涉及设备发现的多播操作必须正确获取与释放 `MulticastLock`,注意不同 Android 版本行为差异。
- 文件读写遵循分区存储(Scoped Storage)规范,不假设有任意路径的文件系统访问权限。
- Android 端界面不提供"创建群组"入口,仅提供"加入群组"(自动发现 + 手动输入IP)。

## 构建与验证命令

```bash
# 初始化/编译核心工作区
cargo build

# 运行核心库测试(会话隔离、去重、路径发现等单元测试)
cargo test -p core

# Windows 客户端本地开发运行
cd tauri-app && cargo tauri dev

# Windows 客户端构建
cd tauri-app && cargo tauri build

# Android 端 Rust 交叉编译产物生成(供 JNI 加载)
cd core-ffi && cargo build --release --target <android-target>
```

## 提交前检查清单

- [ ] 是否明确对应任务分解文档中的某一个顶层任务编号
- [ ] 是否未跨越角色边界(Relay 逻辑未混入 Leaf/core-ffi)
- [ ] 是否复用了前置任务已提供的接口,而非重复实现
- [ ] `cargo build` 与相关 `cargo test` 是否通过
- [ ] 任务分解文档中对应条目是否已勾选更新
