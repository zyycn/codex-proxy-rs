# 插件页面

仅在创建或修改插件页面时读取，同时使用[开发入口](../../cpr-dev-guide/SKILL.md)的页面分支

- 沿用已有插件页面与 [管理端主题](../../../../docs/theme.md#界面文案与信息层级) 的设计和文案层级，详细帮助使用现有 Popover；页面 PR 按 [界面验证](../../../../CONTRIBUTING.md#界面验证) 提供实际截图
- 优先参考独立示例仓库的 `examples/workbench/frontend/src/api/`：业务路由放在 `modules/`，`request.ts` 封装公开宿主桥；按需使用 `@codex-proxy/ui` 包出口，不跨仓导入宿主或 UI 的内部源码
- Vue 页面保留 SFC，逻辑、脚本和配置使用 TypeScript；只实现业务内容，标题和副标题由 `ManagementPage` 交给宿主显示
- 隔离页面通过 `window.codexProxyPlugin.request` 调用已注册管理路由；`models.responses` 通过普通请求链交付 JSON/SSE，支持取消且不暴露 Key 明文。不直接 `fetch` 宿主、不读取管理 Cookie 或借用宿主 Vue 实例
- 管理路由的响应正文由插件业务定义，不默认套用宿主管理 API 的 `{ code, message, data }`；页面与处理器共享实际合同，GET 无正文时不附带 JSON 正文声明
- `host.model.*` 子请求跳过发起插件，不能用它证明本插件中间件／路由已参与；需要完整请求链的示例使用页面模型桥或真实客户端
- JS、CSS、图标等资源随包构建和声明；沿用宿主桥的主题同步。Vite 本地预览不等于已安装插件的热更新，宿主验证仍需重新构建、打包和切换版本
