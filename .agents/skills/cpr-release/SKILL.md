---
name: cpr-release
description: Codex Proxy RS 的版本规划、发布说明、正式或预发行发布与失败恢复；部署重启和普通 Git tag 清理不属于发版
---

# 发版

## 范围与授权

沿用用户确定的版本、范围与授权，准备版本或询问能否发布不执行远程写入；明确发布时完成已授权步骤，无需重复确认

仅打 tag 不自动扩展为完整发版，发布也不包含运行实例升级或合并实验功能

稳定版与 alpha、beta、rc 从 `main` 发布，exp 从对应 `experimental/<用途>` 分支发布

其他仓库使用其发布入口与规则，不套用本仓库路径；规则已在上下文中时不重复读取

## 必须保持的边界

- Codex 兼容性变化按[问题与方案依据](../../../CONTRIBUTING.md#问题与方案依据)核对已有依据与验证范围，缺少依据列为发行风险，不以 CI 通过代替
- `release/publish --check <version>` 只读检查发行准备条件；不带 `--check` 会提交、打 tag、推送并触发工作流，实际发布仍需相应授权
- 同版本失败先区分[续跑原任务与修复源码后重发](references/operations.md#恢复与验收)：前者沿用原提交；后者仅在版本尚未公开时清理失败的版本提交，再生成唯一的版本提交。已发布的 tag 和产物保持不变
- 备版只更新源码的构建版本；通道快照是版本 Release 的 `version.yaml` 附件，验收时核对固定预发行页 `update-index` 指向的公开快照
- 预发行不得覆盖 GitHub Latest 或镜像 `latest`，完成后回读核对
- `release/notes.md` 只承载当前发行的说明；常规文档修改另按开发入口执行[文档检查](../cpr-dev-guide/references/documentation.md)
- 交付版本、提交、Release／工作流链接与实际结果，未完成构建或缺失产物时不称发布成功

## 按任务读取

先落实上述范围与边界，再读取当前阶段的完整指定章节；实际发布包含准备与验收，不能只读执行命令。阶段变化时补齐读取，准备文案不加载执行流程，实际发布前须核对发布入口的完整副作用

| 任务 | 读取范围 |
| --- | --- |
| 推荐版本号 | [基线与版本](references/preparation.md#基线与版本) |
| 编写或翻译发布说明 | [基线与版本](references/preparation.md#基线与版本)、[发布说明](references/preparation.md#发布说明) |
| 检查发行条件 | [发行准备检查](references/preparation.md#发行准备检查) |
| 实际发布 | 先完成[基线与版本](references/preparation.md#基线与版本)、[发布说明](references/preparation.md#发布说明)、[发行准备检查](references/preparation.md#发行准备检查)，再执行[通道核对](references/operations.md#发行通道核对)、[执行发版](references/operations.md#执行发版)与[恢复与验收](references/operations.md#恢复与验收) |
| 失败恢复或验收 | [恢复与验收](references/operations.md#恢复与验收)，先确定已完成步骤，不直接重跑入口 |
| 实验版 | 在对应阶段补读[实验流程](references/experimental.md)；实际发布必须覆盖分支与版本、发布能力、文案和执行与验收 |
| 修复代码、工作流或常规文档 | 使用 [cpr-dev-guide](../cpr-dev-guide/SKILL.md)，有 PR 任务再使用 [cpr-github-pr](../cpr-github-pr/SKILL.md) |
