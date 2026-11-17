---
name: cpr-github-issue
description: 草拟、创建、补充或回复 GitHub Issue，包括缺陷、功能和协作规则反馈；单独排障或修复不自动发布 Issue
---

# Issue 协作

## 目标与范围

从用户 URL、remote 和上下文确认接收仓库，fork 场景区分自己的仓库与上游

草拟只准备正文，明确创建或提交才发布；已有授权直接执行，自动命中技能本身不授权远程写入

补充已有 Issue 按授权更新原帖或追加评论

## 按动作读取与核实

- 新建 Issue 时，读取本仓库的[提交 Issue](../../../CONTRIBUTING.md#提交-issue)及本次匹配的一张表单：[接口问题](../../../.github/ISSUE_TEMPLATE/api-bug-report.yml)、[常规问题](../../../.github/ISSUE_TEMPLATE/general-bug-report.yml)或[功能建议](../../../.github/ISSUE_TEMPLATE/feature-request.yml)；协作规则提案可用普通 Issue
- 补充或回复时先读原帖和讨论，按 [GitHub 回复口吻](../../../CONTRIBUTING.md#github-回复口吻)决定答复内容和是否追问
- 其他仓库读取它自己的相关规则与实际模板，长文档先定位标题再截取，不预读全部模板或 CONTRIBUTING
- 新建或判断重复时查找同场景的 open/closed Issues 和相关 PR，按实际场景核对。已有相同反馈且无新证据时返回原链接，用户仍要求新建则按其要求处理。答复修复状态时核对发布记录，区分已合入和已发布
- 需要技术排查、判断实现方案或修改时执行 [cpr-dev-guide](../cpr-dev-guide/SKILL.md)的领域必读，不能仅凭症状建议跨职责补偿；提交症状反馈不以找到根因为前提
- 引用上游行为或提出方案时执行[问题与方案依据](../../../CONTRIBUTING.md#问题与方案依据)，注明适用版本、来源与缺口

## 新建 Issue 的正文

标题说明症状或期望行为，区分发生问题的部署版本、实测结果、预期结果与根因假设

功能和规则提案说明场景、缺少的能力及建议结果，无需硬套缺陷复现步骤

YAML 表单按 `attributes.label` 和字段顺序组织正文，手工核对必填项、选项与确认项，CLI 不会代做表单校验

未知信息如实标明，影响理解或复现的关键事实先补齐，不提交原始 YAML、占位答案或未确认的勾选项

labels 和 assignees 仅按模板或用户要求设置，并确认实际存在

Issue 可记录复现、排查和变更经过；涉及常规文档修改时执行[文档检查](../cpr-dev-guide/references/documentation.md)

## 发布与收尾

日志和附件只含定位所需的脱敏片段，不公开凭据、完整配置或请求转储，不擅自修改代理、TUN、证书环境取证。附件须获授权且可访问，本机路径不算附件

使用可用连接器或 `gh`，不为提交反馈安装工具、修改仓库配置或启用服务

多行正文通过结构化字段或 UTF-8 文件传递，以下变量须先从已确认的目标与正文赋值：

```bash
gh issue create --repo "$issue_repo" --title "$issue_title" --body-file "$issue_body_file"
```

发布前核对目标和脱敏内容，新建时确认必填项，更新时保留无关原文；不擅自评论、重开或关闭重复 Issue

超时或结果不明确时先查询远端再重试，发布后回读确认并返回链接；仅草拟或发布失败时提供正文并注明未提交
