# Agent Note: Melon 桌面外壳

Status: proposed

[English](2026-08-14-melon-desktop-shell.md) | 中文

## Problem

DeepSeek Harness 目前没有可安装的桌面发行版。用户必须先准备兼容的 Node 与 pnpm 环境，并手动配置提供商，才能通过 DurinDoor 使用它。下游桌面产品需要移除这些主机前置条件，同时不能替换 Cordis 运行时，也不能用复制的源码树增加上游同步成本。

设置页面需要访问原生凭据、下载、文件和子进程。环回地址上的 Harness 页面可信度更低，不能继承这些能力。进程发现也不能证明所有权：Melon 可以连接已有 DurinDoor，但只能停止当前应用会话通过仍存活句柄启动的进程树。

## Proposal

Melon 作为 `deepseek-ai/deepseek-harness` 的增量分支维护。它保留 `master` 分支、全部 `@deepseek-ai/*` 包标识、Cordis 插件运行时、`@deepseek-ai/dsh-agent-loop`、设置、会话、权限、工具和上游 Web 应用。产品改动仅位于 `apps/melon-desktop`、发布与同步工具、面向产品的品牌接缝和一个中央主题覆盖中。

私有工作区包 `@bloodf/melon-desktop` 使用 Tauri v2，包含内置设置页和 Rust `ConnectionController`。其精简命令接口负责报告状态、探测托管或外部 DurinDoor 连接、激活一个选定模型，以及关闭当前会话启动的子进程。所有修改操作通过控制器状态串行执行。

只有设置页所在 webview 获得生成的自定义命令权限。能力文件不包含 `remote.urls` 授权。激活后，同一窗口导航到环回地址的 Harness 页面，且不提供 Tauri 命令桥。Rust 负责端点校验、网络探测、凭据存储、下载、解压、原子配置和进程控制。

可选凭据通过服务名 `com.bloodf.melon` 存入操作系统原生凭据库；凭据库不可用时只允许会话内使用，绝不降级为明文持久化。Cordis YAML 只引用 `MELON_DURINDOOR_API_KEY`，不包含密钥字节。

托管 DurinDoor 使用按目标平台构建并验证的 zip，其中包含 DurinDoor、准备好的生产依赖闭包和 Node 20.20.2。载荷构建在隔离的 `DATA_DIR` 中运行必需的包脚本；终端用户安装绝不运行 npm。由于 DurinDoor CLI 可能终止所选端口的监听进程，Melon 仅在严格确认端口空闲后，才在 `127.0.0.1:20128` 启动它。托管数据位于 Melon 应用数据目录，并且不会被自动删除。

Harness 使用经过验证的 Node 24.19.0 sidecar，以及从源码提交 `47f943859bef60e4160492346772ded9b24f765a` 构建的 pnpm 生产部署闭包。Melon 初始版本为 `0.1.0`；DurinDoor 固定为 3.15.2，对应 npm git head `1c14989f8ec6a56cce1df1bb2806e8ba885012f7`。`apps/melon-desktop/runtime/runtime-pins.json` 是这些固定版本和官方校验和引用的机器可读来源。

每个二进制发行版保留上游 MIT 许可证，并为 DeepSeek Harness、DurinDoor、Node.js、Tauri 和捆绑的生产依赖生成声明。发布资产包含这些声明和校验和。

## Alternatives considered

**独立包装仓库。** 该方案减少与上游源码的重叠，但会把产品品牌、Web 验证、运行时闭包构建和发布证据拆到多个仓库。增量分支让这些改动与实际发布的上游源码树一起接受审查。

**原生重写 Harness。** 该方案让 Tauri 获得完全控制，但会重复 Cordis 智能体运行时、设置、会话、工具和 Web 行为。Melon 保留一个智能体循环，并仅把 Tauri 用作打包和可信设置基础设施。

**向 Harness 页面授予远程命令权限。** 该方案简化单窗口接线，但会把凭据、文件系统、下载和进程命令暴露给环回地址提供的内容。导航改为跨越严格的能力隔离。

**基于 PID 的子进程所有权。** 持久化 PID 可跨控制器重启，但可能陈旧或被复用，也不能证明进程由 Melon 启动。当前会话内仍存活的进程树句柄是唯一所有权证据。

## Acceptance criteria

当原生安装包无需系统 Node 或 pnpm 即可运行、两种 DurinDoor 设置路径都能激活真实的暂存 Harness、密钥不进入文件或日志、只有受跟踪的进程树被停止、远程 Harness 内容无法调用设置命令，并且按目标构建的载荷和安装包可依据提交的固定版本与发布校验和完成验证时，本提案视为已实现。

真实 Tauri 界面还必须在 1440×960 和 1024×768 下通过已提交的 Modern Relay Gauntlet，满足 WCAG 2.2 AA 对比度，并保持上游 Harness 交互不变。

## Risks

DurinDoor 和 Node 固定版本的更新节奏与上游 Harness 不同。更新它们需要重新构建原生载荷、生成不可变哈希、重跑生命周期和安装包检查，并在激活成功前保留上一个有效运行时。

仅省略远程 URL 授权并不能限制 Tauri 自定义命令。构建必须通过 `tauri_build::AppManifest::commands` 生成逐命令权限，并只授予本地设置能力。

上游可能移动少量面向产品的接缝，或更改 CLI 参数解析。同步拉取请求必须优先保留上游运行时行为，只在记录的接缝重新应用 Melon，并在合并前重跑集成和视觉证据。
