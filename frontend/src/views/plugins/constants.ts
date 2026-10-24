import type { PluginCatalogStatus } from './utils/catalog'
import type { PluginObserverEvent } from '@/api'
import { computed } from 'vue'
import { t } from '@/i18n'

interface PluginRequestStage {
  label: string
  scope: 'none' | 'model' | 'provider'
  globalLabel?: string
}

export const PLUGIN_CONFIGURATION_MAX_BYTES = 48 * 1024

export const PLUGIN_OBSERVER_EVENT_LABELS = computed<Record<PluginObserverEvent, string>>(() => ({
  request_completed: t('plugins.fields.requestCompletionAndUsage'),
  websocket_response: t('plugins.fields.upstreamWebsocketEvents'),
}))

export const PLUGIN_CAPABILITY_LABELS = computed<Record<string, string>>(() => ({
  frontend_authentication: t('plugins.configuration.clientAuthentication'),
  scheduler: t('plugins.fields.requestScheduling'),
  model_router: t('plugins.fields.modelRouting'),
  model_catalog: t('plugins.fields.modelCatalog'),
  retry_policy: t('plugins.fields.retryStrategy'),
  middleware: t('plugins.fields.requestMiddleware'),
  upstream_adapter: t('plugins.fields.upstreamAdapter'),
  observer: t('plugins.fields.eventObservation'),
  command_line: t('plugins.fields.commandLine'),
  management: t('plugins.fields.manageExtensions'),
  maintenance: t('plugins.fields.maintenanceTasks'),
}))

// 编辑入口、范围控件与摘要共用阶段描述；能力和阶段的合法组合由宿主校验。
export const PLUGIN_REQUEST_STAGES = computed<Record<string, PluginRequestStage>>(() => ({
  http: { label: t('plugins.fields.httpRequest'), scope: 'none', globalLabel: t('plugins.fields.allHttpRequests') },
  websocket: { label: t('plugins.fields.websocketMessages'), scope: 'none', globalLabel: t('plugins.fields.allWebsocketMessages') },
  service: { label: t('plugins.fields.serviceCall'), scope: 'none', globalLabel: t('plugins.fields.allServiceCalls') },
  request: { label: t('plugins.fields.requestToStart'), scope: 'model' },
  attempt: { label: t('plugins.fields.everyTry'), scope: 'provider' },
  outbound: { label: t('plugins.fields.outboundPreparation'), scope: 'provider' },
  upstream: { label: t('plugins.fields.upstreamCall'), scope: 'provider' },
  routing: { label: t('plugins.fields.modelRouting'), scope: 'model' },
  scheduling: { label: t('plugins.fields.accountScheduling'), scope: 'provider' },
  retry: { label: t('plugins.fields.retryDecision'), scope: 'provider' },
  observation: { label: t('plugins.fields.eventObservation'), scope: 'provider' },
}))

export const PLUGIN_STATUS_LABELS = computed<Record<PluginCatalogStatus, string>>(() => ({
  unaccepted: t('plugins.toBeInstalled'),
  unconfigured: t('plugins.fields.toBeConfigured'),
  enabled: t('common.status.enabled'),
  disabled: t('plugins.fields.deactivated'),
  pending: t('plugins.fields.waitingForItToTakeEffect'),
  failed: t('common.status.error'),
}))
