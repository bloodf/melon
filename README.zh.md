# Melon 桌面版

[English](README.md) | 中文

Melon 是 DeepSeek Harness 的可安装 Tauri 发行版。它保留上游 Cordis 的
agent/session/tool 运行时与所有 `@deepseek-ai/*` 包不变，新增一个受信任的
首次启动界面：连接既有 DurinDoor URL；本地安装要等签名 payload 安装器落地。
Harness 在打包好的 Node 24 sidecar 中以 pnpm 构建产物方式运行；DurinDoor
为默认模型路由。

## 快速开始

当前是开发者预览，尚未发布未签名安装包。从本仓库启动：

```sh
pnpm install
pnpm run melon:dev
```

首次启动选择 **连接既有 DurinDoor**：粘贴来源 origin（或 `/v1` URL）以及可选
API key。返回 401 时 key 变为必填；无鉴权的 DurinDoor 仅凭 URL 即可使用。
非回环 `http://` URL 会先要求确认明文传输，才会发请求。用户提供的 API key
只保存在操作系统原生凭据库中；凭据库不可用时仅留在会话内存，不会写入磁盘
配置或日志。

界面上的 **本地安装 DurinDoor** 仍是 fail-closed：安全 payload 安装器与
`managedLaunchReady` 描述符尚未交付。不要指望本树会下载组件或在
`127.0.0.1:20128` 启动托管实例。

## 致谢

Melon 基于 DeepSeek Harness 构建。DeepSeek Harness（dsh）是 DeepSeek AI
开发的开源 agent harness，也是 Melon 内嵌并配置的运行时。上游的全部行为、
文档以及 LICENSE 中的 MIT 许可均适用于本 fork。DurinDoor、Node、Tauri 的
第三方声明见 THIRD_PARTY_NOTICES.md。

下方其余内容是上游 DeepSeek Harness 的原始文档，未做改动。

---

# DeepSeek Harness

[English](README.md) | 中文

DeepSeek Harness（`dsh`）是由 [DeepSeek AI](https://deepseek.com) 开发的开源 agent harness（智能体框架）。

它采用**一切皆插件**的架构，并由 [Cordis](https://github.com/cordiverse/cordis) 驱动，其设计参见论文 [_A Programming Paradigm for Spatiotemporal Composability_](https://github.com/cordiverse/paper)。

## 开发者预览

DeepSeek Harness 目前处于 _开发者预览_ 阶段，正在快速迭代。**未来将出现破坏兼容性的变更。**

## 运行

### 通过 `npm` 运行

安装 `Node.js`，然后运行：

```sh
npx @deepseek-ai/dsh web
```

该命令会启动 Web UI，默认地址为 `http://127.0.0.1:3080`。详见 [Web UI 指南](docs/user/guide/index.md)。

### 从源码运行

如需从仓库源码运行：

```sh
git clone https://github.com/deepseek-ai/deepseek-harness.git
cd deepseek-harness
pnpm install
pnpm run build
pnpm dsh web
```

## 社区与支持

- 欢迎通过 [GitHub Discussions](https://github.com/deepseek-ai/deepseek-harness/discussions) 提交反馈或 bug 报告。
- 为你的插件仓库添加 [`dsh-plugin`](https://github.com/topics/dsh-plugin) 话题，便于被发现。
- 欢迎加入 DeepSeek Harness 企微群：扫码添加企微小助手并填写入群问卷，完成后小助手会邀请你入群。

<table>
  <thead>
    <tr>
      <th align="center">企微小助手</th>
      <th align="center">入群问卷</th>
      <th align="center">微信公众号</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td align="center"><img src="assets/community-wecom-assistant.png" alt="DeepSeek Harness 企微小助手二维码" width="180" height="180"></td>
      <td align="center"><a href="https://trtgsjkv6r.feishu.cn/share/base/form/shrcnIt5twSVdLGD52KJBckGCgg"><img src="assets/community-wecom-survey.png" alt="DeepSeek Harness 入群问卷二维码" width="180" height="180"></a></td>
      <td align="center"><img src="assets/community-wechat-official-account.png" alt="DeepSeek Harness 团队微信公众号二维码" width="180" height="180"></td>
    </tr>
  </tbody>
</table>

## 参与贡献

参见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 开发

请先阅读[开发指南](docs/development.md)与[架构文档](docs/architecture.md)。

面向 agent：请遵循 [AGENTS.md](AGENTS.md)。

## 许可证

[MIT](LICENSE)

第三方依赖及其许可证见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
