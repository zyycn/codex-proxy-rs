# 项目协作约定

开发和审查先阅读 [CONTRIBUTING.md](CONTRIBUTING.md)，使用其中的协作流程和审查标准。
模块职责以 [系统架构](docs/architecture.md) 为准，界面约定以 [管理端主题](docs/theme.md) 为准；
没有明确约定时保持与相邻实现一致，不把单次页面反馈扩展成全仓规则。

<!-- CODEGRAPH_START -->
## CodeGraph

In repositories indexed by CodeGraph (a `.codegraph/` directory exists at the repo root), reach for it BEFORE grep/find or reading files when you need to understand or locate code:

- **MCP tools** (when available): `codegraph_explore` answers most code questions in one call — the relevant symbols' verbatim source plus the call paths between them. `codegraph_node` returns one symbol's source + callers, or reads a whole file with line numbers. If the tools are listed but deferred, load them by name via tool search.
- **Shell** (always works): `codegraph explore "<symbol names or question>"` and `codegraph node <symbol-or-file>` print the same output.

If there is no `.codegraph/` directory, skip CodeGraph entirely — indexing is the user's decision.
<!-- CODEGRAPH_END -->

## 工作方式

- 先定位变更所属模块、同类实现和上下游调用关系，再选择最小合理改动。复用已有能力，也避免为尚不存在的需求增加抽象。
- 代码注释使用中文，解释原因与边界；提交信息使用英文，沿用历史中的 Conventional Commits 格式。
- 按变更范围执行验证，记录命令、结果和缺口。跳过的测试不算通过；界面与集成行为需要对应运行证据，构建通过不能替代实际验收。
- 审查请求默认只读。修复、提交、推送和合并按用户当前授权执行；“本地验证通过”不等于用户已经审阅批准。
- 不读取或输出无关凭据，不把真实密钥、代理认证、账号令牌或请求转储放入提交、截图及审查报告。

## Code Review Rules

### 审查范围

- 确认目标分支与当前变更范围，结合调用方、被调用方及已有测试审查；结论对应实际审查的提交或工作区状态。
- 按 CONTRIBUTING.md 的维度检查设计、正确性、安全、复杂度、性能、可读性、验证、界面和文档，深度与变更风险相称。
- 重点核对现有架构和数据合同：共享业务事实是否重复解释，异步与并发操作是否破坏状态，权限和敏感数据是否越界，协议适配是否影响兼容性。
- 修改既有特殊分支时先查场景和历史；放宽检查、修改测试或新增例外需要业务依据，不能仅以让检查通过为理由。

### 代码与界面一致性

- 对重复逻辑、冗余状态和多余抽象给出具体位置及影响；建议复用时指出已有实现，不能仅凭行数或语法相似判定质量。
- UI 变更对照项目基础组件、主题和同类页面，检查交互、状态、响应式与可访问性。局部问题是否应在共用组件修复，按职责判断，不一律禁止局部样式。
- 核对截图、预览和实际交互证据；未检查的状态明确列为验证缺口，不推断视觉验收已经通过。

### 审查输出

- 用中文报告本次变更新引入或加重的、可定位的问题，包含文件位置、触发场景或规范依据、影响和最小修复方向。
- 区分必须修复的问题、可选建议和验证缺口。个人偏好标为建议，不作为必须修复项；没有问题时不要凑数。
- 严重程度按实际影响判断，不为绕过工具的优先级过滤而升级普通规范问题。说明审查和验证的实际覆盖范围，AI 未报告问题不等于维护者批准。
