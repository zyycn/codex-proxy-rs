import type { OpsError } from '@/api/modules/usage'

const FAILURE_CLASS_LABELS: Record<string, string> = {
  invalid_request: '请求不合法',
  unsupported: '请求能力不受支持',
  unauthorized: '认证失败',
  auth_failed: '认证失败',
  permission_denied: '上游权限不足',
  policy_denied: '策略拒绝',
  model_not_found: '模型不存在',
  no_available_provider: '没有可用 Provider',
  no_eligible_account: '没有符合条件的账号',
  account_capacity_unavailable: '账号容量不足',
  provider_infrastructure_unavailable: 'Provider 基础设施不可用',
  rate_limited: '上游限流',
  quota_exhausted: '账号额度已耗尽',
  upstream_unavailable: '上游服务不可用',
  upstream_error: '上游响应错误',
  transport: '上游传输失败',
  protocol: '上游协议错误',
  unavailable: 'Provider 暂不可用',
  timeout: '请求超时',
  cancelled: '请求已取消',
  process_terminated: '进程终止',
  internal_error: '内部错误',
  failed: '请求失败',
}

const COMPONENT_LABELS: Record<string, string> = {
  model_request: '模型请求',
  routing: '路由调度',
  account_probe: '账号连接测试',
}

const SOURCE_LABELS: Record<string, string> = {
  model_request: '模型请求记录',
  ops_event: '运维事件',
}

export interface OpsErrorPresentation {
  summary: string
  failureClassLabel: string
  componentLabel: string
  sourceLabel: string
}

/** 将运维错误的稳定机器字段转换为中文摘要；原始诊断仍由调用方单独展示。 */
export function presentOpsError(record: OpsError): OpsErrorPresentation {
  const failureClassLabel = mappedOrOriginal(FAILURE_CLASS_LABELS, record.failureClass, '未分类错误')
  const componentLabel = mappedOrOriginal(
    COMPONENT_LABELS,
    record.metadata.component || record.kind,
    '未知组件',
  )
  const sourceLabel = mappedOrOriginal(SOURCE_LABELS, record.metadata.source, '未知来源')
  const statuses = [
    statusText('客户端', record.clientStatusCode),
    statusText('上游', record.upstreamStatusCode),
  ].filter(Boolean)
  const statusSuffix = statuses.length > 0 ? `（${statuses.join('，')}）` : ''
  const providerCode = record.providerErrorCode?.trim()
  const codeSuffix = providerCode ? ` [${providerCode}]` : ''

  return {
    summary: `${componentLabel}：${failureClassLabel}${codeSuffix}${statusSuffix}`,
    failureClassLabel,
    componentLabel,
    sourceLabel,
  }
}

function mappedOrOriginal(
  labels: Record<string, string>,
  value: string | null | undefined,
  fallback: string,
) {
  const normalized = value?.trim()
  return normalized ? labels[normalized] || normalized : fallback
}

function statusText(owner: string, status: number | null) {
  return typeof status === 'number' ? `${owner} HTTP ${status}` : ''
}
