# Melon Desktop

[English](README.md) | 中文

Melon Desktop 是 DeepSeek Harness 可安装版 Melon 的私有 Tauri 工作区包。它保留上游 Cordis 运行时和 `@deepseek-ai/*` 包，并负责可信的桌面设置界面。

## 开发

在仓库根目录运行完整包检查：

```sh
pnpm run melon:check
```

`pnpm run melon:dev` 启动 Vite 设置页面和 Tauri 应用。生成的 Node sidecar 与暂存 Harness 资源均被忽略；打包脚本根据固定并验证的运行时输入创建它们。

## 信任模型

Tauri 首先启动标签为 `setup` 的一次性窗口。生成的应用权限仅向该标签授予 `status`、`probe`、`activate` 和 `shutdown`，能力文件不包含远程 URL 授权。激活将销毁设置窗口，并创建独立且无授权的 `main` Harness 窗口。返回“连接设置”时将反向执行原生生命周期，而不是在有权限的 webview 中导航。

四个命令处理器均已注册。`status` 报告当前空控制器状态；在进程所有权实现完成前，`shutdown` 可安全调用；在第 2 阶段提供网络、持久化和进程行为前，`probe` 与 `activate` 返回类型化的未实现错误。设置界面通过控制器适配器调用状态、探测、激活和显式重新配置。只有原生应用最终退出负责自动关闭；React 卸载和可拦截的退出请求不会停止子进程。

## 限制

该包仅面向桌面端。它不替换 DeepSeek Harness 的设置、会话、工具、权限或智能体行为，也不向环回地址或远程 Harness 内容授予 Tauri 命令。
