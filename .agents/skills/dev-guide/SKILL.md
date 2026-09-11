---
name: dev-guide
description: 当前仓库开发指南。修改或排查本仓库的 Rust 网关、Vue 管理端、账号与出站代理、接口协议、部署、CI、发布或文档时使用，定位权威规范和验证入口。Use for development in the repository containing this skill; not for generic Rust/Vue questions in unrelated repositories.
---

# 开发指南

先读根目录的 [AGENTS.md](../../../AGENTS.md) 和 [CONTRIBUTING.md](../../../CONTRIBUTING.md)。
本技能只组织工作入口，架构、协议和视觉规则从下列权威文档读取，不维护第二份副本。

## 按任务读取

| 任务 | 文档入口 |
| --- | --- |
| 用户能力、快速开始 | [README](../../../README.md) |
| 模块职责、调用链、状态归属 | [系统架构](../../../docs/architecture.md) |
| HTTP 路由、请求与响应、导入导出 | [API 文档](../../../docs/api.md) |
| 管理端组件、主题 Token、视觉验证 | [管理端主题](../../../docs/theme.md) |
| 部署、客户端配置、备份与恢复 | [部署说明](../../../deploy/README.md) |
| 数据迁移、冻结清单与本地测试库 | [迁移说明](../../../backend/migrations/README.md) |
| 验证命令与提交规范 | [贡献与审查](../../../CONTRIBUTING.md) |
| 创建或审查 PR | [github-pr](../github-pr/SKILL.md) |
| 整理或提交 Issue | [github-issue](../github-issue/SKILL.md) |
| 发布说明、版本规划与发版 | [release](../release/SKILL.md) |

## 执行顺序

1. 从仓库根目录确认工作区、当前任务范围和相关运行实例；保留已有改动。
2. 按 `AGENTS.md` 的 CodeGraph 约定定位入口、调用链和职责归属，再阅读相关实现。
3. 结合上表读取本次需要的文档，先找可复用的同类实现，再修改所属模块。
4. 文档和运行结果不一致时，追查当前源码、配置与版本，区分实现缺陷和文档过期；同步受影响的说明。
5. 按 `CONTRIBUTING.md` 验证并报告结果。后端 Cargo 命令从 `backend/` 执行，或显式指定该目录的 manifest。

发布任务按 `release` 技能读取当前版本、发布入口和工作流，发布说明语言遵循 `CONTRIBUTING.md`。
