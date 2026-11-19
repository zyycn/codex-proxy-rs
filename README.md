<!-- prettier-ignore -->
<div align="center">

<img src="frontend/public/favicon.svg" alt="Codex Proxy RS" width="80" height="80" />

# Codex Proxy RS

面向 Codex 的自托管多账号 AI 网关

[![CI](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zyycn/codex-proxy-rs?display_name=tag&sort=semver&style=flat-square)](https://github.com/zyycn/codex-proxy-rs/releases)
[![GHCR](https://img.shields.io/badge/GHCR-codex--proxy--rs-2496ED?logo=docker&logoColor=white&style=flat-square)](https://github.com/zyycn/codex-proxy-rs/pkgs/container/codex-proxy-rs)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg?style=flat-square)](LICENSE)

[快速预览](#快速预览) · [快速开始](#快速开始) · [文档](#文档) · [社区](#社区) · [贡献者](#贡献者) · [Star History](#star-history) · [支持项目](#支持项目) · [许可证](#许可证)

</div>

> [!NOTE]
> 生成请求支持 [OpenAI Responses](docs/api.md#responses-请求与传输)、[Chat Completions](docs/api.md#chat-completions-请求) 和 [Anthropic Messages](docs/api.md#messages-请求) 三种协议

## 快速预览

| 登录信息 | 默认值 |
| --- | --- |
| 地址 | <https://codex-proxy-rs.ainz.cc> |
| 账号 | `admin@cpr.local` |
| 密码 | `039c18de2aeac46d23ead7766bb07bbbe3747233c66a128e` |

## 快速开始

在已安装 Docker Engine、Docker Compose Plugin、curl 和 OpenSSL 的 Linux amd64/arm64 主机上运行

```bash
curl -fsSL https://raw.githubusercontent.com/zyycn/codex-proxy-rs/main/deploy/install.sh -o install.sh && bash install.sh
```

[安装脚本选项](deploy/README.md#安装脚本选项) · [macOS 发行包](https://github.com/zyycn/codex-proxy-rs/releases) · [Windows 安装](deploy/README.md#windows-二进制部署) · [已有部署升级](deploy/README.md#镜像升级与源码构建)

## 文档

| 主题 | 文档 |
| --- | --- |
| 部署与使用 | [部署、备份与恢复](deploy/README.md) · [客户端接入](deploy/README.md#客户端配置) |
| 接口集成 | [API 参考](docs/api.md) · [模型目录](docs/api.md#模型目录) · [模型定价](docs/api.md#模型定价) |
| 使用插件 | [安装、配置与使用](docs/plugins.md) |
| 开发插件 | [SDK 与合同](backend/crates/gateway-plugin/sdk/README.md) · [打包工具](backend/apps/plugin-cli/README.md) · [官方示例](https://github.com/zyycn/codex-proxy-plugins/blob/main/README.md) |
| 开发共用组件 | [组件库说明](https://github.com/zyycn/codex-proxy-ui/blob/main/README.md) · [组件参考](https://github.com/zyycn/codex-proxy-ui/blob/main/playground/components/index.md) · [Playground](https://zyycn.github.io/codex-proxy-ui/) |
| 开发宿主 | [开发与源码联调](docs/development.md) · [系统架构](docs/architecture.md) · [管理端主题](docs/theme.md) · [数据库迁移](backend/migrations/README.md) |
| 参与协作 | [贡献、审查与验证](CONTRIBUTING.md) |

## 社区

问题反馈与功能建议请使用 [GitHub Issues](https://github.com/zyycn/codex-proxy-rs/issues)，填写要求见 [贡献说明](CONTRIBUTING.md#提交-issue)

感谢 [LINUX DO](https://linux.do) 社区提供开放、友善的技术交流平台

## 贡献者

[<img src="https://contrib.rocks/image?repo=zyycn/codex-proxy-rs&amp;columns=10" alt="查看 GitHub 贡献者列表" width="480">](https://github.com/zyycn/codex-proxy-rs/graphs/contributors)

## Star History

<a href="https://simrepo.dera.page/#zyycn/codex-proxy-rs&amp;Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://simrepo.dera.page/svg?repos=zyycn/codex-proxy-rs&amp;type=Date&amp;theme=dark">
    <img src="https://simrepo.dera.page/svg?repos=zyycn/codex-proxy-rs&amp;type=Date" alt="Codex Proxy RS 的 GitHub Star 增长趋势" width="560">
  </picture>
</a>

## 支持项目

如果 Codex Proxy RS 帮助到你了，欢迎请我喝杯咖啡 ☕

[![Support me on Ko-fi](docs/donations/ko-fi.svg)](https://ko-fi.com/zyycn)

[<img src="docs/donations/wechat-tips.png" alt="WeChat appreciation code" width="138">](docs/donations/wechat-tips.png)&nbsp;
[<img src="docs/donations/alipay.png" alt="Alipay QR code" width="138">](docs/donations/alipay.png)

## 许可证

本项目基于 [Apache License 2.0](LICENSE) 开源
