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

## 按任务读取

只读当前阶段和指定章节，先定位标题再截取，不预读整份 CONTRIBUTING、工作流或所有参考文件；实际发布前仍须核对发布入口的完整副作用

| 任务 | 读取范围 |
| --- | --- |
| 推荐版本号 | [基线与版本](references/preparation.md#基线与版本) |
| 编写或翻译发布说明 | [基线与版本](references/preparation.md#基线与版本)、[发布说明](references/preparation.md#发布说明) |
| 检查发行条件 | [发行准备检查](references/preparation.md#发行准备检查) |
| 实际发布 | 完成准备，再读[通道核对](references/operations.md#发行通道核对)和[执行发版](references/operations.md#执行发版) |
| 失败恢复或验收 | [恢复与验收](references/operations.md#恢复与验收)，先确定已完成步骤，不直接重跑入口 |
| 实验版 | 在对应阶段补读[实验流程](references/experimental.md)的版本、文案或验收章节 |
| 修复代码、工作流或常规文档 | 使用 [cpr-dev-guide](../cpr-dev-guide/SKILL.md)，有 PR 任务再使用 [cpr-github-pr](../cpr-github-pr/SKILL.md) |

## 必须保持的边界

- `release/publish` 会提交、打 tag、推送并触发工作流，没有只读预演模式，不能用于探测或演练
- 同名版本、超时或中断先查本地与远端状态，不删已发布 tag、不移动 tag、不强推掩盖冲突
- 预发行不得覆盖 GitHub Latest 或镜像 `latest`，完成后回读核对
- `release/notes.md` 只承载当前发行的说明，常规文档只写当前状态，不追加版本历史；其变更执行[文档检查](../cpr-dev-guide/references/documentation.md)
- 交付版本、提交、Release／工作流链接与实际结果，未完成构建或缺失产物时不称发布成功
