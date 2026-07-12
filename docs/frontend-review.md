# 前端工程审计(frontend)

> 审查于 `feat/postgres-redis-migration` 工作树。对象:`frontend/`(Vue 3.5 + TypeScript 6 + Vite 8/rolldown + Tailwind 4 + Pinia 3,管理台 SPA)。规模实测:73 个 `.vue` + 46 个 `.ts` 共 119 文件、15,643 行。文中路径均相对 `frontend/`;统计类结论(any 数量、文件行数、引用计数)均由 `rg`/`wc` 实测,`vue-tsc -b --force` 实际跑过(exit 0)。

先说结论:底子好于预期。TS strict 全家桶开启且 typecheck 进了 CI、目录分层与命名纪律好、全项目唯一一处 `v-html` 过了 DOMPurify、echarts 正确 dispose、Pinia 用得克制。真正的债集中在三件事:**工具链只有 prettier 没有 ESLint(也没有测试)**、**类型系统在 API 边界断层(146 处显式 `: any`,域实体零 interface,strict 形同虚设地向视图层泄漏 any)**、**401 会话过期前端完全没处理(拦截器里那句 "redirecting to login" 是假的)**。三者都可控,不需要重写。

## P0 — 工程门禁与真 bug

### 1. 无 ESLint:唯一的静态门禁是 prettier
- 现状:`package.json` scripts 只有 `dev/build/preview/format/format:check`,devDependencies 无任何 eslint 包,全仓无 eslint 配置文件;CI(`.github/workflows/ci.yml:90`)只跑 `format:check` + `build`。
- 问题:格式化不是 lint。当前代码里 lint 本可拦下的东西都在裸奔:146 处 `: any`、10 处 `console.*`、21 处 `catch (err: any)`、未 await 的 promise 无人检查。团队/AI 协作写入时没有任何质量回压。
- 建议:补 flat config——`eslint` + `eslint-plugin-vue` + `typescript-eslint` + `eslint-config-prettier`。规则重点:`vue/recommended`、`@typescript-eslint/no-explicit-any`(先 warn)、`no-console`(allow `warn`/`error`)、type-checked 模式下的 `no-floating-promises`。加 `lint` script 并进 CI。一次性设施,收益长期。

### 2. API 层类型断层:strict 全开,但域实体零 interface
- 现状:`src/api/modules/` 7 个模块中只有 `system.ts` 定义了类型并用 `request<SystemVersion>` 传参;其余全部 `params?: any` / `data: ApiPayload` + 隐式 `Promise<any>`(`request.ts:103` 的默认泛型是 `<T = any>`)。`usage.ts` 8 个函数全 `any`;`useDashboard.ts:22` 直接 `export function useDashboard(): any` 且内部 10 个 `ref<any>`;Account/ApiKey/UsageRecord/DashboardSummary 这些核心实体在前端**不存在任何 interface**。实测:`: any` 146 处、`as any` 4 处、`ref<any` 17 处,波及 46/119 个文件。
- 问题:tsconfig 的 strict(见"做得好的")在 API 边界被显式 any 击穿——后端明明是 serde `rename_all = "camelCase"` 的完备结构体(`backend/src/api/admin/*`、`backend/src/telemetry/{usage,ops}/types.rs`),类型信息在 HTTP 边界全部丢弃,后端改字段前端零编译报错,契约漂移只能靠肉眼。`views/usage/constants.ts` 里 20 个 `record: any` 取值函数就是这个断层的下游症状。
- 建议:为每个模块补请求/响应 interface,`request<T>` 处收口。优先 usage 与 accounts(any 最密)。手写可行;更稳的路线是从后端类型生成(ts-rs / schemars→OpenAPI→openapi-typescript),因为 Rust 侧类型是现成的。

### 3. 401 会话过期无任何前端处理,那条日志在撒谎
- 现状:`src/api/request.ts:74` 对 401 只有 `console.warn('Unauthorized, redirecting to login...')`——**没有任何 redirect 代码**。路由守卫(`src/router/index.ts:25`)只在导航时校验,且 `sessionChecked` 置位后不再重查(`src/stores/modules/auth.ts:21`)。
- 问题:会话 cookie 过期后,用户停留的页面上所有请求持续 401 失败,各处 toast 报"请求失败",永远不会回到登录页;刷新才会触发守卫。对管理台这是高频真实路径(挂着 dashboard 过夜)。
- 建议:响应拦截器里 401 → 重置 auth store + `router.replace('/login')`(排除 login/auth-status 自身的 401,防循环;注意 request.ts 与 router 的循环 import,可用动态 import 或事件解耦)。十几行改动,收益最大。

## P1 — 结构与一致性

### 4. 零测试
- 现状:全 frontend 无 vitest/@vue/test-utils/任何 `*.spec.*`;CI 无前端测试步骤。
- 问题:管理台低覆盖可以接受,但连纯函数都没有测试就亏了——`views/usage/constants.ts` 的 `formatCompactTokenCount`(K/M 边界、toFixed 位数)、`usageModelDisplay`(primary/secondary 优先级瀑布)是典型的"便宜且值得"的单测对象,现在这类逻辑只能靠 UI 手点回归。
- 建议:vitest 起步,先只测 `src/utils/`、`views/*/constants.ts` 的纯函数与 `useIdSet`/`useAsyncAction`,不碰组件渲染测试。

### 5. `views/usage/constants.ts`(379 行)名不副实
- 现状:名为 constants,实际是 2 个列定义 + 20 余个取值/格式化/分类**函数**(`tokenTotal`、`usageRecordType`、`visibleRequestText`、`extractInputText`……),且参数全 `record: any`。
- 问题:命名与内容不符,复用者找不到;`docs/architecture.md:337` 对 backend `utils/` 已有"按角色词表命名、禁堆泛用文件"的明文规范,前端同理。它也是 any 密度最高的单文件(20 处)。
- 建议:拆 `formatters.ts` + `record-helpers.ts`(或并入 composable);配合 P0-2 定义 `UsageRecord` 后,这里的 any 自然消解。

### 6. 巨型 composable / 组件已到拆分临界
- 现状(top 5 实测):`layout/components/SystemUpdateModal.vue` 600 行、`views/dashboard/composables/useDashboard.ts` 447 行、`views/accounts/composables/useAccountMutations.ts` 442 行、`components/base/BaseScrollbar.vue` 438 行、`views/accounts/composables/useAccountConnectionTest.ts` 424 行。
- 问题:`useDashboard` 把状态、加载、竞态控制、格式化、图标映射全揉进一个函数并返回 `any`;`useAccountMutations` 装了 9 个 action + 3 个 modal 开关 + 表单状态。尚未失控,但每加一个功能都在加重。
- 建议:不必立刻大拆。`useDashboard` 先把 `metricCards`/formatter 等纯函数移出 composable 体;`useAccountMutations` 可按 create/delete/refresh 分组。列在此处主要是"别再往里加"。

### 7. 同名 composable 不同语义:filters 双轨
- 现状:`views/accounts/composables/useAccountFilters.ts` 是 server-side 分页(`bindAccountLoader` 注入加载回调,搜索 debounce 后重新请求);`views/api-keys/composables/useApiKeyFilters.ts` 是 client-side 全量过滤 + 内存分页(`api-keys.ts:5` 的 `getApiKeys()` 无分页参数,一次拉全)。
- 问题:两个 `useXxxFilters` 名字一致、心智模型完全不同;`bindAccountLoader` 的 setter 注入让 composable 之间有隐式调用顺序耦合(loader 未 bind 时静默不加载)。api-keys 数据量小,全量拉取本身没错,错在同名掩盖了差异。
- 建议:低成本方案是命名区分(如 `useAccountServerFilters` / 注释标明 client-side);或统一为传入 loader 参数而非事后 bind。

### 8. `ApiError` 定义了却没导出
- 现状:`src/api/request.ts:25` 的 `ApiError` 带 `status/code/requestId`,但未 `export`;`src/composables/useSystemUpdate.ts:267` 只能 `(error as { status?: unknown }).status` 鸭子类型取值。
- 问题:调用方无法 `instanceof ApiError` 收窄,好不容易带上的 `x-request-id`(与后端 request_id 链路呼应)在 UI 层取不到类型化的值。
- 建议:`export class ApiError`,useSystemUpdate 改用 instanceof;错误 toast 可顺带展示 requestId 方便对账后端日志。

### 9. 双轨错误处理:useAsyncAction(toast)与裸 console.error 并存
- 现状:`src/composables/useAsyncAction.ts` 是正确的统一通道(loading 互斥 + toast + 可选 rethrow),accounts/api-keys 的 mutation 大多走它;但 `useDashboard.ts:53,67,84`、`useAccountMutations.ts:320,373` 仍是 `try { … } catch { console.error }`,`request.ts:55-83` 的拦截器里还有 5 处按状态码打 console。
- 问题:dashboard 加载失败用户无感知(只进 console);拦截器里的 console 与 reject 后调用方的处理重复,信息价值低。
- 建议:dashboard 的加载失败至少给一次 toast 或页面 error 态;拦截器的 switch/console 块可整体删除(ApiError 已带全部信息)。引入 ESLint `no-console` 后这些会自然浮出。

## P2 — 打磨项,视精力而定

### 10. 死代码:`BaseSwitch.vue`
- `src/components/base/BaseSwitch.vue`(60 行)全仓 0 引用(PascalCase 与 kebab-case 都查过)。删除即可。其余低引用 base 组件核实过都有真实使用(见"核实过的误报")。

### 11. 路由无懒加载,登录页也进主包
- `src/router/routes.ts` 全部静态 import,产物主 chunk 599 KB(未压缩;echarts 已手动拆出 524 KB + zrender,`vite.config.ts` 的 codeSplitting 配置是对的)。内网管理台可接受;要做的话把 5 个 view 改 `component: () => import(...)` 即可,登录页首屏收益最明显。

### 12. GET 请求 `_t=Date.now()` 缓存穿透
- `src/api/request.ts:45-50` 给所有 GET 挂时间戳查询参数。老派 hack,污染 URL 且对 HTTP 缓存语义是绕过而非声明。正统做法是后端 admin API 返回 `Cache-Control: no-store`(代理后端本就该如此),前端删掉这段。

### 13. i18n 缺失——记为决策而非欠债
- 49/73 个 `.vue` 硬编码中文文案,无 vue-i18n。单语管理台完全合理,不建议现在引 i18n;仅提示:若未来有开源/多语诉求,补收成本随文件数线性增长,届时是一次机械但大面积的改造。

### 14. a11y 基本盘尚可,modal 缺 focus 管理
- 42 处 `aria-*`、`BaseModal.vue:82` 有 `aria-modal="true"`,自研组件有基本意识;但 BaseModal 未见 focus trap/焦点归还实现(未运行时验证)。管理台优先级低,做 base 组件迭代时顺手补。

### 15. 小件
- `// @env browser` 非标准注释头出现 5 处(`src/composables/useDownload.ts:1` 等),无任何工具消费它;引入 ESLint 后删除或换成真实配置。
- `src/api/index.ts` 桶文件 `export *` 全量再导出:目前无命名冲突,但两个模块一旦撞名会静默覆盖;模块数少,维持现状可接受,新增模块时留意。

## 做得好的(记录一下,免得反复怀疑)

- **TS 严格度配置满分**:`tsconfig.app.json` 开了 `strict` + `noUnusedLocals` + `noUnusedParameters` + `verbatimModuleSyntax`,`build` 脚本前置 `vue-tsc -b` 且 CI 必跑——typecheck 是硬门禁(实测通过)。问题在边界的显式 any(P0-2),不在配置。
- **分层与命名纪律**:`api/modules` 按域一文件、`views/<域>/{components,composables,constants.ts}` feature-first、通用件收敛在 `components/base`(16 个);组件 PascalCase、composable `useXxx`、view 入口 `index.vue`,抽查未发现违例。`src/utils/` 仅 3 个文件且各司其职(async/date/markdown),没有垃圾桶化——`docs/architecture.md` §6.4 对 utils 的告诫在前端是兑现的。
- **XSS 防线**:全项目 `v-html` 仅 1 处(`SystemUpdateModal.vue:387`),源头 `src/utils/markdown.ts` 走 `DOMPurify.sanitize`(ADD_ATTR 白名单),外链统一注入 `rel="noreferrer"`。
- **资源与竞态**:`components/charts/BaseChart.vue` 在 `onBeforeUnmount` 正确 dispose echarts 实例;`useDashboard` 用 `trendRequestId` 防过期响应回写;`useIdSet.run()` 天然防并发重复提交。
- **useAsyncAction 抽象正确**:loading 互斥 + `withMinimumDuration` 防闪烁 + toast 错误统一 + 可选 rethrow,是该有的形状,问题只是覆盖不全(P1-9)。
- **Pinia 用得克制**:全局只有 auth/ui 两个 store,列表/表单状态留在 view-local composables——服务端状态没有被错误地塞进全局单例,三个列表视图状态互不重叠。
- **ui store 主题切换工程质量高**:View Transition API circle reveal + 不支持时 fallback + `prefers-reduced-motion` 降级 + timeout/finally 双保险清理;`tokens.css` 235 个 `--cp-*` 变量统一光/暗主题,71 个组件用 Tailwind utility,仅 8 个 scoped style 块,样式组织一致。
- **构建与供应链卫生**:`dist/` 未进 git;echarts/zrender 手动分包;`pnpm-workspace.yaml` 配了 `minimumReleaseAge` 供应链防护;依赖版本新且锁定。
- **ApiError 携带 `x-request-id`**:与后端 request_id 链路闭环呼应(导出后更完整,见 P1-8)。

## 核实过的误报

- **"strict 没开、any 是隐式的"**——不成立。strict 全开、`vue-tsc` 通过,146 处 any 全是显式标注。这改变的是修法(补类型定义,而非改编译器配置),不改变问题本身。
- **低引用 base 组件是死代码**——除 `BaseSwitch`(0 引用)外不成立:BaseTextarea(1 处)、BaseCheckbox(2 处)等均有真实调用点,是"备件少用"不是"没人用"。
- **"stores 只有 2 个 = 状态管理缺失/该集中"**——不成立。列表页状态本就该 view-local,实查无跨视图重复状态;往 Pinia 里搬反而制造共享可变状态。
- **`_t` 时间戳导致行为错误**——不成立,行为正确(配合 `withCredentials` 的 admin GET 总是新鲜数据),只是手段不优雅,降级列 P2-12。
- **手写 UI 组件库是重复造轮子失控**——不成立。16 个 base 组件体量可控(最大 BaseScrollbar 438 行),与 tokens.css 主题体系一体;引入 Element Plus 反而带来 bundle 与主题适配成本。

## 建议动手顺序

1. **P0-3(401 跳登录)**先修:改动最小(request.ts 十几行)、用户可感收益最大。
2. **P0-1(ESLint + CI)**一次性设施:规则先 warn 后 error 渐进,避免一次性 146 个 any 报错淹死。
3. **P0-2 + P1-5 结对做**:先定义 `UsageRecord`/`Account`/`ApiKey`/`DashboardSummary` interface(usage 最密集,优先),顺手把 `usage/constants.ts` 拆掉——any 总量预计能砍半以上,`useDashboard(): any` 一并消解。
4. **零风险小件随任意 PR 带走**:export ApiError(P1-8)、删 BaseSwitch(P2-10)、删 `@env browser` 注释与拦截器 console 块(P2-15、P1-9 前半)。
5. **P1-4(vitest)**在类型补齐后做,纯函数先行。
6. 其余 P1/P2 按精力。
