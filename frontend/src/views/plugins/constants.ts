import type { PluginCatalogStatus } from './utils/catalog'
import type { PluginObserverEvent } from '@/api'

interface PluginRequestStage {
  label: string
  scope: 'none' | 'model' | 'provider'
  globalLabel?: string
}

export const PLUGIN_CONFIGURATION_MAX_BYTES = 48 * 1024

export const PLUGIN_OBSERVER_EVENT_LABELS: Record<PluginObserverEvent, string> = {
  request_completed: '请求完成与用量',
  websocket_response: '上游 WebSocket 事件',
}

export const PLUGIN_CAPABILITY_LABELS: Record<string, string> = {
  frontend_authentication: '客户端认证',
  scheduler: '请求调度',
  model_router: '模型路由',
  model_catalog: '模型目录',
  retry_policy: '重试策略',
  middleware: '请求中间件',
  upstream_adapter: '上游适配器',
  observer: '事件观察',
  command_line: '命令行',
  management: '管理扩展',
  maintenance: '维护任务',
}

// 编辑入口、范围控件与摘要共用阶段描述；能力和阶段的合法组合由宿主校验。
export const PLUGIN_REQUEST_STAGES: Record<string, PluginRequestStage> = {
  http: { label: 'HTTP 请求', scope: 'none', globalLabel: '所有 HTTP 请求' },
  websocket: { label: 'WebSocket 消息', scope: 'none', globalLabel: '所有 WebSocket 消息' },
  service: { label: '服务调用', scope: 'none', globalLabel: '所有服务调用' },
  request: { label: '请求开始', scope: 'model' },
  attempt: { label: '每次尝试', scope: 'provider' },
  upstream: { label: '上游调用', scope: 'provider' },
  routing: { label: '模型路由', scope: 'model' },
  scheduling: { label: '账号调度', scope: 'provider' },
  retry: { label: '重试决策', scope: 'provider' },
  observation: { label: '事件观察', scope: 'provider' },
}

export const PLUGIN_STATUS_LABELS: Record<PluginCatalogStatus, string> = {
  unaccepted: '待安装',
  unconfigured: '待配置',
  enabled: '已启用',
  disabled: '已停用',
  pending: '等待生效',
  failed: '需要处理',
  incompatible: '不兼容',
}
