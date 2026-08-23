<!-- prettier-ignore -->
<div align="center">

<img src="../frontend/public/favicon.svg" alt="Codex Proxy RS" width="72" height="72" />

# 管理端主题系统

**基于 Seed、Map、Alias 与 Component Token 的运行时主题架构**

[设计原则](#设计原则) · [Token 模型](#token-模型) · [运行时架构](#运行时架构) · [主题编辑器](#主题编辑器) · [扩展指南](#扩展指南)

</div>

主题系统为 Codex Proxy RS 管理端提供浅色、深色、预置配色与自定义配色能力。实现借鉴 Ant Design 的
Token 分层与十阶色板算法，但不引入 `antd`、React、`ConfigProvider` 或 CSS-in-JS；Vue 组件继续通过
CSS Variables 与 Tailwind CSS 4 utilities 消费主题。

> [!IMPORTANT]
> 主题的目标是调整颜色、密度、圆角与层级，不改变现有页面结构和产品语义。默认主题必须保持既有视觉基线，
> 成功、警告、错误和业务数据色不会因为品牌色变化而失去原有含义。

## 设计原则

- **所选即所得**：`colorPrimary` 始终保留用户选择的 Seed 原色；只有 Hover、Active、弱背景和文字色使用派生值。
- **主色与表面分离**：品牌色负责交互和强调，页面、容器、浮层与文字由独立的背景和文本 Seed 派生。
- **语义独立**：`success`、`warning`、`error`、`info` 与 `link` 拥有各自 Seed，不随品牌色隐式改变。
- **无边设计**：默认依靠表面色差、间距和轻阴影表达层级；边框只用于焦点、错误和必要分隔。
- **运行时可定制**：用户输入在浏览器中实时派生，因此使用 CSS Variables，不使用构建时 SCSS 变量。
- **单一事实源**：Store 只保存最小配置，所有 Map、Alias 与未覆盖的 Component Token 均由纯函数生成。
- **可访问性优先**：文字、焦点和实心控件在 Alias 层执行对比度保护，并尊重 `prefers-reduced-motion`。

## 架构概览

```mermaid
flowchart LR
  A[Pinia 持久化配置] --> B[resolveTheme]
  B --> C[Seed Token]
  C --> D[Map Token]
  D --> E[Alias Token]
  E --> F[Component Token]
  F --> G[#cp-runtime-theme]
  G --> H[Vue / Tailwind / ECharts]

  B --> I[Theme Editor 草稿]
  I --> J[Shadow DOM 预览]
  J --> K[首页画板 / 组件概览]
```

核心职责保持分离：

| 层 | 文件 | 职责 |
| --- | --- | --- |
| 领域模型 | [`theme/index.ts`](../frontend/src/theme/index.ts) | 类型、规范化、纯派生算法与最终 Token 输出 |
| 设计基线 | [`theme/constants.ts`](../frontend/src/theme/constants.ts) | 默认值、预置主题、中性色面、阴影与可编辑白名单 |
| 浏览器适配 | [`theme/browser.ts`](../frontend/src/theme/browser.ts) | 将解析结果提交为根作用域运行时样式表 |
| 状态 | [`stores/modules/theme.ts`](../frontend/src/stores/modules/theme.ts) | 持久化配置、系统明暗偏好、切换动作与动画 |
| 编辑器状态 | [`useThemeEditor.ts`](../frontend/src/views/theme/composables/useThemeEditor.ts) | 草稿、修改计数、恢复与保存 |
| 样式桥接 | [`styles/index.css`](../frontend/src/styles/index.css) | 将 CSS Token 暴露为 Tailwind CSS 4 utilities |
| 稳定语义 | [`styles/tokens.css`](../frontend/src/styles/tokens.css) | 不参与主题派生的业务常量 |

`theme/index.ts` 不访问 DOM，`theme/browser.ts` 不包含派生规则，Theme Store 不复制算法。类型与算法集中在
`theme/index.ts`，不额外拆分 `types.ts`。

## Token 模型

主题采用四层模型：

```text
Seed Token → Map Token → Alias Token → Component Token
```

### Seed Token

Seed 是用户配置与持久化的最小事实：

```ts
type ThemeMode = 'system' | 'light' | 'dark'
type ThemeColorId = 'relay-blue' | 'deep-teal' | 'signal-violet' | 'graphite' | 'custom'

interface ThemeCustomization {
  seed?: Partial<ThemeSeedOverrides>
  component?: Partial<ThemeComponentOverrides>
  tokenOverrides?: Partial<ThemeTokens>
}
```

| 类别 | Seed |
| --- | --- |
| 品牌 | `colorPrimary`，由预置 ID 或自定义 HEX 提供 |
| 功能色 | `colorSuccess`、`colorWarning`、`colorError`、`colorInfo`、`colorLink` |
| 中性色 | `colorTextBase`、`colorBgBase` |
| 尺寸 | `fontSize`、`sizeUnit`、`sizeStep`、`controlHeight` |
| 风格 | `borderRadius`、`shadowStrength` |
| 组件尺寸 | `tableRowHeight`、`cardBorderRadius` |

持久化键为 `codex-proxy-rs-theme`。Store 不保存完整色板或 CSS Variables，损坏的模式、颜色与自定义值会在
初始化时规范化并回退到默认配置。

### Map Token

品牌色和功能色使用 `@ant-design/colors` 从一个 Seed 生成十阶色板。主色派生规则为：

| 角色 | 规则 |
| --- | --- |
| `colorPrimary` | 始终使用 Seed 原色 |
| Hover | 浅色取 P5，深色取暗色算法 P8 |
| Active | 浅色取 P7，深色取暗色算法 P6 |
| 实心 Hover | Seed 向白混合 6% |
| 实心 Active | Seed 向黑混合 8% |
| 弱背景 | 从当前容器表面与主色混合，避免低亮度 Seed 产生脏灰色块 |
| 文字与描边 | 从色板取值后，对相邻 Surface 执行对比度校正 |

背景与文本 Seed 进入独立的 Surface Map，生成：

- `colorBgLayout / Container / Elevated / Spotlight / Mask`
- `colorFillSecondary / Tertiary / Quaternary`
- `colorText / Heading / Secondary / Tertiary / Quaternary / Disabled`
- `colorBorder / BorderSecondary / Split / Shadow`

默认中继蓝使用固定的石墨中性色基线。其他预置通过各自的浅色、深色 `colorBgBase` 与 `colorTextBase`
建立完整气质。自定义主色只向中性表面注入少量色温：浅色混入 2.8%，深色从默认暗面与暗色色板背景混合；
用户显式设置背景或文字 Seed 后，自动配套值立即让位。

### Alias Token

Alias 按视觉角色命名，使用 `--cp-` 命名空间，并优先对齐 Ant Design 的公开 Token 词汇：

| 角色 | CSS Token | 主要消费者 |
| --- | --- | --- |
| 品牌交互 | `--cp-color-primary*` | 主操作、选中态、焦点与强调 |
| 链接 | `--cp-color-link*` | 普通文本链接 |
| 表面 | `--cp-color-bg-*` | 页面、容器、浮层和遮罩 |
| 填充 | `--cp-color-fill-*` | 轨道、弱背景和静态层级 |
| 文字 | `--cp-color-text*` | 标题、正文和辅助信息 |
| 选择 | `--cp-control-item-bg-active*` | Segmented、Select 和选择控件 |
| 焦点 | `--cp-control-outline` | 键盘焦点和输入反馈 |
| 功能色 | `--cp-color-info / success / warning / error-*` | 系统反馈与状态 |

Alias 层执行以下保护：

- 普通文字与背景的目标对比度为 4.5:1。
- 控件边界与相邻 Surface 的目标对比度为 3:1。
- 极亮的实心按钮背景自动切换为深色文字，避免白底白字。
- 校正只修改最终 Alias，不反写用户保存的 Seed。

### Component Token

当组件需要独立演进时，使用 `component[-part][-variant][-state]-property` 命名，不创造含义重复的全局别名。

| 组件 | 代表 Token |
| --- | --- |
| Button | `--cp-button-primary-color / bg / hover-bg / active-bg` |
| Input | `--cp-input-bg / hover-bg / active-bg / error-active-bg` 与对应 Shadow |
| Menu | `--cp-menu-item-selected-bg` |
| Table | `--cp-table-row-bg / stripe-bg / hover-bg / selected-bg / height` |
| Card | `--cp-card-bg / border-radius / shadow` |
| Layout | `--cp-layout-sider-bg / shadow` |
| Scrollbar | `--cp-scrollbar-thumb-bg / hover-bg` |
| BrandMark | `--cp-brand-mark-bg` |

应用内品牌图标使用 [`AppBrandMark.vue`](../frontend/src/components/AppBrandMark.vue)：浅色模式取
`colorBgSpotlight`，深色模式取 `colorBgElevated`，保持中性暗面与白色图形；浏览器 favicon 继续使用固定黑白
图标，不随主题改变。

### 稳定业务语义

下列颜色不属于通用主题皮肤，保留在 `styles/tokens.css`：

- 账号“正常”状态 `colorStatusNormal*`
- 推理标识色 `colorReasoning`
- 账号套餐标识色 `accountPlan*`

它们可以分别拥有浅色和深色值，但不会被品牌 Seed 重写。图表数据系列同样按数据语义管理，不把所有曲线染成
主色。

## 预置主题

| ID | 名称 | 主色 | 表面方向 |
| --- | --- | --- | --- |
| `relay-blue` | 中继蓝 | `#5983F4` | 默认石墨控制面，清晰可信 |
| `deep-teal` | 深海青 | `#0E7C72` | 氧化铜与深水玻璃，冷静低饱和 |
| `signal-violet` | 古风色 | `#A0583D` | 赭石、宣纸与松烟墨，温厚克制 |
| `graphite` | 石墨 | `#525B66` | 工作室灰纸与铅笔粉末，近单色层级 |

预置主题只改变颜色画像，不改变字号、密度、圆角或布局。所有预置与自定义主题走同一条派生链路，不存在页面级
特判。

## 运行时架构

### 启动顺序

[`main.ts`](../frontend/src/main.ts) 在 Vue 挂载前初始化主题：

```text
createApp
  → app.use(pinia)
  → persistedstate 同步水合 Theme Store
  → themeStore.initializeTheme()
  → app.use(router / auth)
  → app.mount('#app')
```

这样首个 Vue 组件渲染时已经具备正确主题，不需要在 `index.html` 增加 bootstrap 脚本，也不维护第二套本地存储
解析逻辑。

### CSS Variables 提交

全局主题由 `<head>` 中唯一的运行时样式节点承载：

```html
<style id="cp-runtime-theme">
:root[data-theme='dark'][data-theme-color='relay-blue'] {
  color-scheme: dark;
  --cp-color-bg-layout: #0B111C;
  /* 其余 Map、Alias 与 Component Token */
}
</style>
```

根元素只保留 `data-theme` 与 `data-theme-color` 状态，不写整套内联 Token。运行时样式表比逐项调用
`style.setProperty()` 更集中、可检查，也避免在 DOM 属性中形成超长变量串。全局弹窗通过 Teleport 挂到 `body`
后仍继承根变量。

> [!NOTE]
> Theme Editor 预览是例外：草稿 Token 以内联变量写在影子环境的局部根节点上，仅影响预览，不污染已保存主题。

### 切换与图表

- 用户主动切换时，以点击位置为圆心运行 View Transition；不支持时使用 180ms 颜色过渡。
- 初始化、系统主题变化或减少动态效果时直接提交，不播放扩散动画。
- 每次有效提交只增加一次 `themeRevision`，相同签名不会重复刷新。
- `useThemeColor()` 在正式页面读取根 CSS Variables，在影子预览中优先读取注入的局部 Token。
- 图表 Option 在主题 revision 或预览 Token 改变后重算，`BaseChart` 通过 `setOption` 更新现有 Canvas。

## 主题编辑器

主题编辑器位于一级路由 `/theme`，采用一屏工作台：左侧编辑，右侧预览，顶部保留全局操作。桌面端高度固定为
`100dvh - 3rem`，切换编辑层级或预览类型时不改变外框高度。窄屏头部操作允许自然换行；手机端隐藏实时预览，
只保留 Token 编辑，避免在有限宽度内渲染不可操作的缩放画板。

### 编辑能力

| 区域 | 能力 |
| --- | --- |
| 全局 / 颜色 | 模式、预置、自定义主色、功能色、链接、基础文本与背景、派生变量查看 |
| 全局 / 尺寸 | 字号、基础间距、尺寸步长、控件高度 |
| 全局 / 风格 | 通用圆角、阴影强度 |
| 组件 | Action、Form、Selection、Surface、Data Display、Feedback、Navigation、Layout |
| 工作流 | 搜索、单项恢复、撤销草稿、恢复默认、保存并应用 |

Component Token 只开放白名单字段。未覆盖项始终继续使用全局 Seed 与 Alias 算法，避免主题配置逐渐退化成一份
无法维护的完整 CSS 快照。

### 草稿与保存

编辑器维护 `saved` 与 `draft` 两份状态：

1. 输入只更新草稿和局部预览。
2. 修改计数按模式、主题色、Seed、组件值和 Token override 分项统计。
3. “撤销草稿”恢复到最近一次已保存配置。
4. “保存并应用”先规范化草稿，再原子更新 Theme Store。

### 隔离预览

[`ThemePreviewScope.vue`](../frontend/src/views/theme/components/ThemePreviewScope.vue) 创建开放 Shadow Root，复制应用
样式，并把预览内容 Teleport 到影子根中。预览可独立切换浅色和深色，不受外层主题影响。

- **首页画板**复用真实 `DashboardContent` 和固定 fixture，不请求接口，也不启动自动刷新。
- **组件概览**展示基础组件、表格、空状态、骨架、浮层和菜单等关键状态。
- 画板固定为 `1600 × 1808`，使用 CSS `zoom` 重排，不使用 `transform: scale()` 长期缩放文字。
- 空白区域可拖拽，滚轮以指针为锚点缩放，并提供缩小、100%、放大和适应画板操作。
- 编辑面板和组件概览统一使用 `BaseScrollbar`，滚动条空闲时自动隐藏。

## 样式与命名约定

### CSS Token

- 全局 Token：`--cp-color-bg-container`、`--cp-font-size`。
- Component Token：`--cp-table-row-hover-bg`、`--cp-input-active-shadow`。
- 业务 Token：命名中明确业务域，例如 `--cp-account-plan-pro-bg`。
- 禁止继续引入 `accent`、`soft`、`current`、`subtle` 等与现有角色重叠的平行词汇。

### Vue 与 Tailwind CSS 4

组件优先使用 `bg-cp-*`、`text-cp-*`、`shadow-cp-*`、`rounded-cp-*` 等由 `@theme inline` 暴露的 utility：

```vue
<section class="rounded-cp-card bg-cp-bg-container text-cp-text shadow-cp-card">
  ...
</section>
```

仅当值需要在 CSS 函数、SVG 或局部派生中参与计算时，直接读取 `var(--cp-*)`。不在页面组件中重新实现色阶、
对比度或明暗算法。

### 视觉状态

- 静态填充、Hover、Active 与 Selected 必须使用不同角色，不能复用一个变量制造所有层级。
- Input 的 Hover 与 Active 只改变内部填充；外圈反馈保持同色同宽，错误状态使用独立 Error Token。
- 表格斑马纹使用 `table-row-stripe-bg`，不借用 `row-hover-bg`。
- Card 和选项默认不增加装饰性边框；键盘焦点必须保留可见反馈。
- 阴影保持中性，`shadowStrength` 只调节层级强弱，不给阴影染品牌色。

## 扩展指南

### 增加预置主题

1. 在 `ThemeColorPresetId` 增加稳定 ID。
2. 在 `THEME_COLOR_PRESETS` 声明名称、描述、主色 Seed 与可选浅暗表面画像。
3. 不新增页面特判，确认预置可通过同一 `resolveTheme()` 派生。
4. 在浅色、深色和系统模式下检查文本、浮层、Input、表格与图表。

### 增加全局 Seed 或 Alias

1. 在 `theme/index.ts` 的领域类型中声明字段。
2. 在规范化函数中定义合法输入边界。
3. 在 Map 或 Alias 层集中派生，并加入 `ThemeTokens` 完整输出。
4. 如需 Tailwind utility，在 `styles/index.css` 的 `@theme inline` 中映射。
5. 只在确实需要用户控制时加入 Theme Editor；派生细节默认只读。

### 增加 Component Token

1. 先确认全局 Alias 无法准确表达组件职责。
2. 使用 `component[-part][-variant][-state]-property` 命名。
3. 在 `ThemeComponentMap` 和 `toThemeTokens()` 中提供默认派生值。
4. 需要开放编辑时，加入对应组件目录和可编辑白名单。
5. 基础组件消费 Token，页面不得再写第二套局部常量。

> [!WARNING]
> 不要把任意 HEX、阴影或尺寸加入 `styles/tokens.css` 作为“临时修复”。可换肤值必须进入派生链；只有稳定的
> 业务语义与首帧安全 fallback 可以留在静态样式中。

## 验证

前端改动至少执行：

```bash
cd frontend
pnpm run lint
pnpm run typecheck
pnpm run build
git diff --check
```

视觉验收覆盖：

- 四个预置、自定义 HEX、浅色、深色和跟随系统模式。
- 页面、容器、浮层、输入框、表格、分页、主按钮、选中态、焦点和品牌图标。
- 草稿隔离、保存刷新恢复与 Teleport 弹窗。
- 首页画板缩放清晰度、组件概览滚动、ECharts 网格线和 Skeleton 动效。
- 键盘操作、颜色之外的选中反馈和 `prefers-reduced-motion`。

## 上游参考

- [Ant Design 色彩规范](https://github.com/ant-design/ant-design/blob/977d8e037a4841bb847b8a40ffd1f79b23264826/docs/spec/colors.zh-CN.md)
- [Ant Design 暗黑模式](https://github.com/ant-design/ant-design/blob/977d8e037a4841bb847b8a40ffd1f79b23264826/docs/spec/dark.zh-CN.md)
- [Ant Design 主题定制](https://github.com/ant-design/ant-design/blob/977d8e037a4841bb847b8a40ffd1f79b23264826/docs/react/customize-theme.zh-CN.md)
- [Seed 到 Map](https://github.com/ant-design/ant-design/blob/977d8e037a4841bb847b8a40ffd1f79b23264826/components/theme/themes/shared/genColorMapToken.ts)
- [Map 到 Alias](https://github.com/ant-design/ant-design/blob/977d8e037a4841bb847b8a40ffd1f79b23264826/components/theme/util/alias.ts)
- [`@ant-design/colors` 生成器](https://github.com/ant-design/ant-design-colors/blob/89b4a5b7e989b792610087abe855bf4a2fb1d322/src/generate.ts)
