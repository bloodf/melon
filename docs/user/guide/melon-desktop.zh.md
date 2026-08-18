# 使用 Melon Desktop

[English](melon-desktop.md) | 中文

Melon Desktop 是 DeepSeek Harness 的可安装 Tauri 发行版。它保留上游 Cordis 运行时、`@deepseek-ai/*` 包、设置、会话、工具、权限与智能体行为，只拥有桌面连接界面。标签为 `setup` 的 Tauri 窗口是唯一可调用原生命令的界面；环回地址上的 Harness 页面不会获得 Tauri 能力。本指南描述源码版本中当前的设置流程。

## 前置条件

- Linux x64、macOS x64 或 arm64，或 Windows x64
- 一个你可以访问的 DurinDoor 端点：可以是你自己启动的本地 `127.0.0.1` 服务，也可以是带可选 API 密钥的远端 URL

## 首次运行

首次启动时 Melon 显示选择界面，提供两个选项：

- **本地安装 DurinDoor** — 在本机下载并运行托管的 DurinDoor。
- **连接现有 DurinDoor** — 用一个 URL 和可选的 API 密钥指向已有的 DurinDoor。

源码版本中 **本地安装 DurinDoor** 路径不可用。构建与打包托管运行时是单独的步骤，会产出已签名安装包；当前源码树不包含托管本地安装流程、自动更新通道或已签名安装包。请在自行启动 DurinDoor 之后，对 `127.0.0.1` URL 使用 **连接现有 DurinDoor**。

## 连接现有 DurinDoor

1. 选择 **连接现有 DurinDoor**。
2. 输入 URL。表单会把 URL 规范化，使其以 `/v1` 结尾。URL userinfo、查询字符串或片段中的凭据，以及 `/v1` 之后的任何路径，都会被拒绝。
3. 对无密钥的 DurinDoor，把 API 密钥留空；对需要认证的，则填入密钥。模型发现请求返回 401 时，表单会让你回到该步骤并把密钥字段标记为必填。
4. 当 URL 是 `http://` 且主机不是环回地址时，表单会在发起任何请求之前要求你显式确认不安全的 HTTP。环回 `http://`（`127.0.0.1`、`localhost`、`::1`、`[::1]`）不需要确认。
5. 提交。Melon 会探测 URL 并加载模型目录。

### Melon 探测的内容

对每个外部连接，Melon 都会针对规范化后的管理 URL（去掉结尾 `/v1` 后的 URL）发起以下请求：

- `GET <management>/api/health`：当宿主机暴露该路径时调用。成功响应确认这是一个 DurinDoor 实例；通过反向代理时该路径缺失只会产生警告，不会作为硬失败。
- `GET <management>/api/v1/realtime/auth`：携带 bearer 密钥调用。`200` 接受该密钥；`401` 让表单回到上一步；`404` 或 `405` 表示凭据校验不可用。
- `GET <baseUrl>/models`：使用同一 bearer（或无密钥时的占位符 `sk_durindoor`）调用。必须返回非空 `data` 数组。

探测过程不会发送任何计费的聊天补全请求。

## 选择模型

模型选择器会列出端点返回的 ID。在选择一个模型之前，**启动 Melon** 按钮一直处于禁用状态。如果端点无法验证密钥，选择器会把 API 密钥标记为“未验证”；此时选择模型并启动仍然有效。

激活后，第二次连接、切换模型，以及 Web UI 中上游的模型选择器都仍然可用。Melon 只设置初始的 DurinDoor 路由。

## 激活

激活会向 `<app-data>/harness/` 写入一份生成的 Cordis 补丁和一份不含密钥的连接文档，绑定一个环回端口，在打包的 Node 24 sidecar 下启动已暂存的 `dsh --profile web`，并设置 `MELON_DURINDOOR_API_KEY` 与 `DSH_HOME`，等待 HTTP 200 后再返回。设置窗口会自行替换为环回地址上的 Harness 页面。

填入的 API 密钥会写入系统钥匙串，服务名 `com.bloodf.melon`，账户 id 使用规范化后的端点。`connection.json` 中只保存该账户 id。如果凭据服务不可用，密钥会保留在会话内存中，下次启动时表单会再次询问。之后未带新密钥的激活会把已保存的密钥重新注入子进程环境。

在源码版本中，激活需要打包的 Node 24 sidecar 与已暂存的 Harness 运行时描述符；这两者都未提交进仓库，构建与打包是单独的步骤。在这些资源被暂存之前，当前激活路径会返回类型化的 `not-implemented` 错误。

## 已保存的连接与重新配置

连接被提交后，原生菜单中的 **连接设置…** 会把窗口带回到设置界面。表单会显示已保存的 URL、模型和账户 id，不会回显 API 密钥。**重试启动** 会复用已保存的连接；**更改连接** 会回到选择界面。

## 限制

- 源码版本不提供托管本地安装路径。
- 种子安装包没有已签名安装包、没有自动更新，也没有托管本地安装流程。
- Melon 不会修改 DurinDoor 源码、`~/.9router` 数据库，也不会停止当前会话内没有由它启动的进程。
- 品牌固定：侧栏字标 “Melon”、“M” 标志、页脚 “Built on DeepSeek Harness”。主题覆盖位于 [`packages/client/web/src/melon-theme.css`](../../client/web/src/melon-theme.css)；不出现散落的颜色字面量。
- 提供方设置仍由上游模型选择器与[模型配置指南](./providers.md)负责；Melon 只设置初始的 DurinDoor 路由。

## 开发

包检查、Vite 开发服务器、sidecar 暂存与运行时暂存都位于 [apps/melon-desktop/README.md](../../apps/melon-desktop/README.md)。使用 `pnpm run melon:dev` 启动 Tauri 界面，使用 `pnpm run melon:check` 跑定向测试与契约套件。
