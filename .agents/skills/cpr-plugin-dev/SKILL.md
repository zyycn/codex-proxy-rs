---
name: cpr-plugin-dev
description: 开发、排查或打包 Codex Proxy RS 独立网关插件；仅改宿主 Runtime、安装器或管理端时使用 cpr-dev-guide，不适用于 Codex 插件或 Vue 插件
---

# 网关插件开发

插件在独立项目中通过公开 SDK 实现业务，不向宿主业务模块添加专属流程

同时使用 [cpr-dev-guide](../cpr-dev-guide/SKILL.md)的对应任务分支，已加载则复用，不递归重读

## 范围与合同

- 确认用途、工程位置、目标宿主版本与平台，沿用已有工程；位置无法推断时只补齐该信息，不把插件源码塞进宿主 workspace
- 核对运行宿主、[SDK 版本](../../../backend/crates/gateway-plugin/sdk/Cargo.toml)、[宿主支持清单](../../../backend/crates/gateway-plugin/runtime/plugin-host-compatibility.json)及所用 CLI，清单、进程和能力版本分别判断
- `contributes` 声明处理器，`bindings` 选择挂载与匹配范围；安装成功不等于功能生效。当前清单拒绝 `permissions`，安装者完整信任插件，`trustedProcess` 不提供操作系统沙箱或访问域隔离，见[完整信任](../../../backend/crates/gateway-plugin/sdk/docs/manifest.md#完整信任)
- 涉及 Codex 时按[参考仓库优先级](../cpr-dev-guide/SKILL.md#参考仓库的优先级)先查官方 Codex 源码，三方仓库仅作参考；宿主能力以目标版本的 SDK 为准

## 按当前步骤读取

只读取命中行与链接的指定章节，长文档先列标题再截取；不预读全部 SDK、示例、前端和打包资料

| 当前工作 | 入口 |
| --- | --- |
| 选择能力或确认能力缺口 | [能力与方法](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#能力与方法)、所需能力章节、[插件职责边界](../cpr-dev-guide/references/plugin-boundaries.md) |
| 清单与 Rust 处理器 | [实现参考](references/implementation.md)、所用 SDK 类型；只有修改清单字段时读[清单](../../../backend/crates/gateway-plugin/sdk/docs/manifest.md)对应章节 |
| 创建或修改页面 | [页面参考](references/frontend.md)，不需要页面的插件不创建前端 |
| 准备工程或联调依赖 | [工程与依赖](references/development.md#工程与依赖)，需要示例时再读[复用示例](references/development.md#复用示例) |
| 构建或打包 | [构建与打包](references/development.md#构建与打包) |
| 实际验收 | [验证与交付](references/development.md#验证与交付)，按实现范围验证 |
| 分发插件 | [发布与安装来源](references/development.md#发布与安装来源)，不自动进入宿主发版流程 |
| 修改插件文档 | [文档检查](../cpr-dev-guide/references/documentation.md)，只写当前用法、合同和限制，不附修复过程 |

## 交付

说明源码与产物位置、兼容范围、能力与绑定、实际验证及缺口，只报告真正生成的安装包或完成的业务验证

修改源码不默认授权在目标实例安装启用、调用真实上游或发布，沿用用户当前授权判断后续动作

PR 任务才使用 [cpr-github-pr](../cpr-github-pr/SKILL.md)，文档检查按开发入口收尾执行
