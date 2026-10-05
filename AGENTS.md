# 项目协作约定

按用户意图和当前会话主动加载并执行仓库技能，不要求用户写出 `$技能名`：

| 任务意图与常见说法 | 必用技能 |
| --- | --- |
| 开发、实现、修复、排查报错、审查问题或代码、修改文档 | [cpr-dev-guide](.agents/skills/cpr-dev-guide/SKILL.md) |
| 提交／提个 PR、准备或更新 PR、看下 PR、回复 PR 讨论、处理 PR 审查意见 | [cpr-github-pr](.agents/skills/cpr-github-pr/SKILL.md)；需要修改时同时执行 cpr-dev-guide |
| 提 Issue、反馈到仓库、把问题或建议整理成 Issue、补充或回复 Issue | [cpr-github-issue](.agents/skills/cpr-github-issue/SKILL.md) |
| 写网关插件、修改插件能力、排查插件接入、打包插件 | [cpr-plugin-dev](.agents/skills/cpr-plugin-dev/SKILL.md)，同时执行 cpr-dev-guide |
| 发版、准备版本、发布 beta／实验版、整理发布说明、打发行 tag | [cpr-release](.agents/skills/cpr-release/SKILL.md) |

结合上下文识别“提交吧”“看看这个问题”等省略说法：普通 Git 提交不自动变成 PR，代码排查不自动变成 Issue；审查与发版准备不代表获准远程写入

只加载当前任务命中的技能、模式和文档章节，复用已有上下文，进入新阶段再补读；链接是按需入口，不是全文或全部参考文件的必读清单

执行所选流程中的问题核实、自审、验证与交付要求，协作流程和审查标准见 [CONTRIBUTING.md](CONTRIBUTING.md)

Issue 和 PR 回复分别按对应技能的[Issue 回复口吻](.agents/skills/cpr-github-issue/SKILL.md#回复口吻)与[PR 回复口吻](.agents/skills/cpr-github-pr/SKILL.md#回复口吻)执行

模块职责以 [系统架构](docs/architecture.md) 为准，界面约定以 [管理端主题](docs/theme.md) 为准；
没有明确约定时保持与相邻实现一致，不把单次页面反馈扩展成全仓规则

<!-- CODEGRAPH_START -->
## CodeGraph

In repositories indexed by CodeGraph (a `.codegraph/` directory exists at the repo root), reach for it BEFORE grep/find or reading files when you need to understand or locate code:

- **MCP tools** (when available): `codegraph_explore` answers most code questions in one call — the relevant symbols' verbatim source plus the call paths between them. `codegraph_node` returns one symbol's source + callers, or reads a whole file with line numbers. If the tools are listed but deferred, load them by name via tool search.
- **Shell** (always works): `codegraph explore "<symbol names or question>"` and `codegraph node <symbol-or-file>` print the same output.

If there is no `.codegraph/` directory, skip CodeGraph entirely — indexing is the user's decision.
<!-- CODEGRAPH_END -->

## 工作方式

- 开发和审查按 [问题与方案依据](CONTRIBUTING.md#问题与方案依据) 先核实问题；涉及 Codex 行为、协议或兼容性时，按[参考仓库优先级](.agents/skills/cpr-dev-guide/SKILL.md#参考仓库的优先级)以官方 Codex 仓库对应版本源码为第一手资料，三方仓库仅作参考
- 先定位变更所属模块、同类实现和上下游调用关系，再选择最小合理改动。复用已有能力，也避免为尚不存在的需求增加抽象。
- 提交前自审最终差异，先处理本次引入的职责混淆、重复规则、冗余状态和无依据的兼容分支；后端按 [后端自审](docs/architecture.md#后端自审) 追踪状态与资源生命周期，不能把自审留给 CI 或 PR 审查者。
- 页面变更先复用已有页面设计与组件，不因个人偏好重排页面或另建交互。文案取舍遵循 [界面文案与信息层级](docs/theme.md#界面文案与信息层级)，页面 PR 必须提供 [实际截图](CONTRIBUTING.md#界面验证)。
- 代码注释使用中文，解释原因与边界；提交信息使用英文，沿用历史中的 Conventional Commits 格式。
- 常规文档只描述当前状态，不记录变更历史；按[文档职责与更新条件](CONTRIBUTING.md#文档职责与更新条件)在所属章节就地修订，分清层次与详略，短段和列表少用句号，交付前逐段检查必要性、归属、重复与当前状态表达
- 按变更范围执行验证，记录命令、结果和缺口。跳过的测试不算通过；界面与集成行为需要对应运行证据，构建通过不能替代实际验收。
- 审查请求默认只读。修复、提交、推送和合并按用户当前授权执行；“本地验证通过”不等于用户已经审阅批准。
- 不读取或输出无关凭据，不把真实密钥、代理认证、账号令牌或请求转储放入提交、截图及审查报告。
- 使用 AI 的 PR 按 [AI 使用披露](CONTRIBUTING.md#ai-使用披露) 写明实际模型型号，不以工具名称代替模型。

## Code Review Rules

### 审查范围

- 确认目标分支与当前变更范围，结合调用方、被调用方及已有测试审查；结论对应实际审查的提交或工作区状态。
- 按[审查标准](CONTRIBUTING.md#审查标准)检查设计、正确性、安全、复杂度、性能、可读性、验证、界面和文档，深度与变更风险相称
- 重点核对现有架构和数据合同：共享业务事实是否重复解释，异步与并发操作是否破坏状态，权限和敏感数据是否越界，协议适配是否影响兼容性。
- 修改既有特殊分支时先查场景和历史；放宽检查、修改测试或新增例外需要业务依据，不能仅以让检查通过为理由。

### 代码与界面一致性

- 对重复逻辑、冗余状态和多余抽象给出具体位置及影响；建议复用时指出已有实现，不能仅凭行数或语法相似判定质量。
- UI 变更对照原页面、项目基础组件、主题和同类页面，检查布局变化是否服务本次需求、是否堆叠重复文案，以及交互、状态、响应式与可访问性。局部问题是否应在共用组件修复，按职责判断，不一律禁止局部样式。
- 核对页面 PR 按 [界面验证](CONTRIBUTING.md#界面验证) 提供的截图与实际交互证据；缺少必需截图时明确要求补齐，未检查的状态列为验证缺口，不推断视觉验收已经通过。

### 审查输出

- 用中文先回答原问题是否成立、解决方向是否合理，依据与判断标准见 [贡献与审查](CONTRIBUTING.md#审查标准)，再报告实现问题和验证缺口。
- 实现问题聚焦本次变更新引入或加重的、可定位的问题，包含文件位置、触发场景或规范依据、影响和最小修复方向。
- 区分必须修复的问题、可选建议和验证缺口。个人偏好标为建议，不作为必须修复项；没有问题时不要凑数。
- 严重程度按实际影响判断，不为绕过工具的优先级过滤而升级普通规范问题。说明审查和验证的实际覆盖范围，AI 未报告问题不等于维护者批准。
