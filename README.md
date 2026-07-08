# LAN Mesh

局域网 Mesh 通信软件 workspace。

## Workspace

- `core/`: 核心通信库，承载网络、协议、路由、文件传输等共享逻辑，不包含 UI。
- `core-ffi/`: Android JNI/FFI 适配层预留 crate，仅暴露 Leaf 角色能力。
- `tauri-app/`: Windows Tauri 客户端预留目录，后续通过本地路径依赖调用 `core`。
- `android-app/`: Android 客户端预留目录，后续通过 JNI 调用 `core-ffi`。

当前初始化完成 `core` 与 `core-ffi` 两个 Rust crate。
