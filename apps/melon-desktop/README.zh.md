# Melon Desktop

[English](README.md) | 中文

Melon Desktop 是 DeepSeek Harness 可安装版 Melon 的私有 Tauri 工作区包。它保留上游 Cordis 运行时和 `@deepseek-ai/*` 包，并负责可信的桌面设置界面。

## 开发

在仓库根目录运行完整包检查：

```sh
pnpm run melon:check
```

`pnpm run melon:dev` 启动 Vite 设置页面和 Tauri 应用。生成的 Node sidecar 与暂存 Harness 资源均被忽略；打包脚本根据固定并验证的运行时输入创建它们。

## Harness 运行时

`pnpm run melon:stage-harness-runtime` 构建上游包和 Web 前端，验证当前发布流程的打包安装，部署 `@deepseek-ai/dsh` 生产依赖闭包，检查包元数据和依赖项，然后将生成的闭包原子发布到 `src-tauri/resources/harness/`。运行 `pnpm run melon:test:harness-runtime` 检查基于夹具的暂存约定。

## DurinDoor 负载

原生发布运行器使用已提交的 npm 锁构建 DurinDoor CLI 与运行时种子。`payload.json` 通过 SHA-256 绑定按可移植路径排序的 `metadata/runtime-seed-manifest.json`；该清单记录每个锁定种子文件在未来应用数据运行时根目录下的相对路径、大小、SHA-256 与可执行位，并固定 `better-sqlite3`、`sql.js` 的包和本机文件路径及版本。描述符不携带可执行脚本或参数，且在原生安装安全合并种子前继续标记 `managedLaunchReady: false`。

## 信任模型

Tauri 首先启动标签为 `setup` 的一次性窗口。生成的应用权限仅向该标签授予 `status`、`probe`、`activate` 和 `shutdown`，能力文件不包含远程 URL 授权。激活将销毁设置窗口，并创建独立且无授权的 `main` Harness 窗口。返回“连接设置”时将反向执行原生生命周期，而不是在有权限的 webview 中导航。

四个命令处理器均已注册。`status` 报告当前空控制器状态；在进程所有权实现完成前，`shutdown` 可安全调用；在第 2 阶段提供网络、持久化和进程行为前，`probe` 与 `activate` 返回类型化的未实现错误。设置界面通过控制器适配器调用状态、探测、激活和显式重新配置。只有原生应用最终退出负责自动关闭；React 卸载和可拦截的退出请求不会停止子进程。

## 限制

该包仅面向桌面端。它不替换 DeepSeek Harness 的设置、会话、工具、权限或智能体行为，也不向环回地址或远程 Harness 内容授予 Tauri 命令。
