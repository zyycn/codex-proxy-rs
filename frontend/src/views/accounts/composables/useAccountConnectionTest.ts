import type { Account, AccountModelsResponse } from '@/api'
import { CheckCircle2, Clock3, Wifi, XCircle } from '@lucide/vue'

import { useEventSource } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { computed, onBeforeUnmount, ref, shallowRef, watch } from 'vue'
import { getAccountModels, refreshAccountModels } from '@/api'
import { API_BASE_URL } from '@/api/constants'
import { toast } from '@/components/base/BaseToast'
import { useIdSet } from '@/composables/useIdSet'
import { errorMessage, withMinimumDuration } from '@/utils/async'
import { formatDateTime, formatTime } from '@/utils/date'

interface ConnectionTestRun {
  accountId: string
  resolve: () => void
}

type ConnectionTestStatus = 'idle' | 'running' | 'success' | 'error'
type ConnectionTestLogTone = 'normal' | 'info' | 'success' | 'danger'

interface ConnectionTestModelOption {
  label: string
  value: string
}

interface ConnectionTestLog {
  key: string
  time: string
  text: string
  tone: ConnectionTestLogTone
  detail: string
}

interface ConnectionTestRequestPayload {
  input?: Array<{ content?: Array<{ type?: string, text?: string }> }>
}

interface ConnectionTestStartEvent {
  type: 'test_start'
  text?: string
  model?: string
}

interface ConnectionTestRequestEvent {
  type: 'request'
  payload?: ConnectionTestRequestPayload
}

interface ConnectionTestStatusEvent {
  type: 'status'
  text?: string
}

interface ConnectionTestContentEvent {
  type: 'content'
  text?: string
}

interface ConnectionTestCompleteEvent {
  type: 'test_complete'
  success: boolean
  error?: string
}

type ConnectionTestFailureSource = 'gateway' | 'provider' | 'upstream'
type ConnectionTestSendState = 'not_sent' | 'sent' | 'ambiguous'

interface ConnectionTestFailureEvent {
  type: 'error'
  source?: ConnectionTestFailureSource
  gatewayErrorCode?: string
  sendState?: ConnectionTestSendState | null
  error?: string
  providerErrorCode?: string | null
  providerErrorType?: string | null
  upstreamStatus?: number | null
  upstreamContentType?: string | null
  upstreamBody?: string | null
}

type ConnectionTestEvent
  = | ConnectionTestStartEvent
    | ConnectionTestRequestEvent
    | ConnectionTestStatusEvent
    | ConnectionTestContentEvent
    | ConnectionTestCompleteEvent
    | ConnectionTestFailureEvent

const CONNECTION_TEST_EVENT_TYPES = new Set<ConnectionTestEvent['type']>([
  'test_start',
  'request',
  'status',
  'content',
  'test_complete',
  'error',
])

const CONNECTION_TEST_FAILURE_TEXT: Record<string, string> = {
  invalid_request: '测试请求不合法',
  unsupported: '当前 Provider 不支持连接测试',
  unauthorized: '账号凭据无效',
  policy_denied: '测试请求被网关策略拒绝',
  model_not_found: '测试模型不存在',
  no_available_provider: '指定账号当前不可用于连接测试',
  account_capacity_unavailable: '指定账号当前没有可用容量',
  provider_infrastructure_unavailable: 'Provider 本地基础设施暂不可用',
  rate_limited: '上游请求过于频繁',
  upstream_unavailable: '上游服务暂不可用',
  timeout: '上游请求超时',
  cancelled: '测试请求已取消',
  internal_error: '网关内部错误',
}

const CONNECTION_TEST_SOURCE_LABEL: Record<ConnectionTestFailureSource, string> = {
  gateway: '网关校验',
  provider: 'Provider 本地准备',
  upstream: '上游响应',
}

function parseConnectionTestEvent(raw: string): ConnectionTestEvent | null {
  const value: unknown = JSON.parse(raw)
  if (!value || typeof value !== 'object' || !('type' in value) || typeof value.type !== 'string')
    throw new TypeError('invalid connection-test event')
  if (!CONNECTION_TEST_EVENT_TYPES.has(value.type as ConnectionTestEvent['type']))
    return null
  return value as ConnectionTestEvent
}

function connectionTestFailureText(event: ConnectionTestFailureEvent) {
  return event.gatewayErrorCode
    ? CONNECTION_TEST_FAILURE_TEXT[event.gatewayErrorCode] || '未分类错误'
    : '测试连接失败'
}

function connectionTestFailureLabel(event: ConnectionTestFailureEvent) {
  return event.source ? CONNECTION_TEST_SOURCE_LABEL[event.source] || '测试失败' : '测试失败'
}

function connectionTestFailureDiagnostics(event: ConnectionTestFailureEvent) {
  return {
    error: event.error ?? null,
    gatewayErrorCode: event.gatewayErrorCode ?? null,
    sendState: event.sendState ?? null,
    upstreamStatus: event.upstreamStatus ?? null,
    providerErrorCode: event.providerErrorCode ?? null,
    providerErrorType: event.providerErrorType ?? null,
    upstreamContentType: event.upstreamContentType ?? null,
    upstreamBody: event.upstreamBody ?? null,
  }
}

export function useAccountConnectionTest(options: { reload: () => Promise<unknown> }) {
  const showConnectionTestModal = shallowRef(false)
  const testingAccount = shallowRef<Account | null>(null)
  const connectionTestStatus = shallowRef<ConnectionTestStatus>('idle')
  const connectionTestModel = shallowRef('')
  const connectionTestContent = shallowRef('')
  const connectionTestLogs = ref<ConnectionTestLog[]>([])
  const connectionTestError = shallowRef('')
  const connectionTestStartedAt = shallowRef('')
  const connectionTestFinishedAt = shallowRef('')
  const connectionTestDurationMs = shallowRef<number | null>(null)
  const testingConnections = useIdSet<string>()
  const loadingConnectionTestModels = shallowRef(false)
  const refreshingConnectionTestModels = shallowRef(false)
  const connectionTestSelectedModel = shallowRef('')
  const connectionTestModelOptions = ref<ConnectionTestModelOption[]>([])
  const connectionTestStreamUrl = shallowRef<string>()
  const {
    data: connectionTestStreamMessage,
    error: connectionTestStreamError,
    eventSource: connectionTestEventSource,
    open: openConnectionTestEventSource,
    close: closeConnectionTestEventSource,
  } = useEventSource(connectionTestStreamUrl, [], {
    autoConnect: false,
    immediate: false,
    withCredentials: true,
    serializer: {
      read: raw => ({ raw }),
    },
  })

  let connectionTestStartedAtMs = 0
  let connectionTestRun: ConnectionTestRun | undefined

  const connectionTestStatusView = computed(() => {
    if (connectionTestStatus.value === 'running') {
      return {
        label: '正在测试',
        description: '正在向所选模型发送请求并接收流式响应',
        icon: Clock3,
        badge: 'bg-cp-info-bg text-cp-info-text',
        iconClass: 'text-cp-info',
      }
    }
    if (connectionTestStatus.value === 'success') {
      return {
        label: '连接正常',
        description: '请求已完成，可在下方查看模型、耗时和事件轨迹',
        icon: CheckCircle2,
        badge: 'bg-cp-success-bg text-cp-success-text',
        iconClass: 'text-cp-success',
      }
    }
    if (connectionTestStatus.value === 'error') {
      return {
        label: '测试失败',
        description: '请求未完成，请在下方查看失败来源与原始诊断',
        icon: XCircle,
        badge: 'bg-cp-error-bg text-cp-error-text',
        iconClass: 'text-cp-error',
      }
    }
    return {
      label: '准备测试',
      description: '选择模型后，点击“开始测试”发送真实请求',
      icon: Wifi,
      badge: 'bg-cp-fill-quaternary text-cp-text-secondary',
      iconClass: 'text-cp-text-quaternary',
    }
  })

  function openConnectionTest(account: Account) {
    abortConnectionTest()
    testingAccount.value = account
    connectionTestSelectedModel.value = ''
    connectionTestModelOptions.value = []
    showConnectionTestModal.value = true
    resetConnectionTest()
    void loadConnectionTestModels(account)
  }

  function resetConnectionTest() {
    connectionTestStatus.value = 'idle'
    connectionTestModel.value = ''
    connectionTestContent.value = ''
    connectionTestLogs.value = []
    connectionTestError.value = ''
    connectionTestStartedAt.value = ''
    connectionTestFinishedAt.value = ''
    connectionTestDurationMs.value = null
    connectionTestStartedAtMs = 0
  }

  function formatConnectionTestDetail(value: unknown) {
    if (value === undefined || value === null || value === '')
      return ''
    if (typeof value === 'string')
      return value
    return JSON.stringify(value, null, 2)
  }

  function connectionTestRequestText(payload?: ConnectionTestRequestPayload) {
    const texts = (payload?.input ?? [])
      .flatMap(item => item.content ?? [])
      .filter(item => item.type === 'input_text' && item.text)
      .map(item => item.text)
    return texts.join('\n')
  }

  function connectionTestLogItem(
    key: string,
    text: string,
    tone: ConnectionTestLogTone = 'normal',
    detail?: unknown,
  ): ConnectionTestLog {
    return {
      key,
      time: formatTime(),
      text,
      tone,
      detail: formatConnectionTestDetail(detail),
    }
  }

  function appendConnectionTestLog(
    text: string,
    tone: ConnectionTestLogTone = 'normal',
    detail?: unknown,
  ) {
    connectionTestLogs.value = [
      ...connectionTestLogs.value,
      connectionTestLogItem(`${Date.now()}-${connectionTestLogs.value.length}`, text, tone, detail),
    ]
  }

  function setConnectionTestLog(
    key: string,
    text: string,
    tone: ConnectionTestLogTone = 'normal',
    detail?: unknown,
  ) {
    const index = connectionTestLogs.value.findIndex(item => item.key === key)
    const next = connectionTestLogItem(key, text, tone, detail)
    if (index === -1) {
      connectionTestLogs.value = [...connectionTestLogs.value, next]
      return
    }
    connectionTestLogs.value = connectionTestLogs.value.map((item, itemIndex) =>
      itemIndex === index ? { ...next, time: item.time } : item,
    )
  }

  function finishConnectionTest(status: 'success' | 'error') {
    connectionTestStatus.value = status
    connectionTestFinishedAt.value = formatDateTime()
    connectionTestDurationMs.value = clamp(
      Date.now() - connectionTestStartedAtMs,
      0,
      Number.POSITIVE_INFINITY,
    )
  }

  function clearConnectionTestRun() {
    const run = connectionTestRun
    connectionTestRun = undefined
    closeConnectionTestEventSource()
    if (run) {
      testingConnections.remove(run.accountId)
      run.resolve()
    }
  }

  function failConnectionTest(message = '测试连接失败') {
    if (connectionTestStatus.value === 'running') {
      recordConnectionTestFailure('failure', '测试失败', message)
    }
    clearConnectionTestRun()
  }

  function recordConnectionTestFailure(
    key: string,
    label: string,
    message: string,
    detail?: unknown,
  ) {
    connectionTestError.value = message
    setConnectionTestLog(key, `${label}：${message}`, 'danger', detail)
    finishConnectionTest('error')
  }

  function handleConnectionTestEvent(event: ConnectionTestEvent) {
    if (event.type === 'test_start') {
      connectionTestModel.value = event.model || connectionTestModel.value
      appendConnectionTestLog(`开始测试 ${connectionTestModel.value || '未选择模型'}`, 'info')
      return
    }
    if (event.type === 'request') {
      setConnectionTestLog('request', '发起请求', 'info', connectionTestRequestText(event.payload))
      return
    }
    if (event.type === 'status' && event.text) {
      appendConnectionTestLog(event.text, 'info')
      return
    }
    if (event.type === 'content' && event.text) {
      connectionTestContent.value += event.text
      setConnectionTestLog('response', '接收响应内容', 'success', connectionTestContent.value)
      return
    }
    if (event.type === 'test_complete') {
      if (event.success) {
        if (!connectionTestContent.value) {
          setConnectionTestLog('response', '响应完成', 'success', '上游已完成，没有返回文本内容')
        }
        appendConnectionTestLog('测试完成', 'success')
        finishConnectionTest('success')
      }
      else {
        recordConnectionTestFailure(
          'test-complete-failure',
          '测试失败',
          '测试连接失败',
          { error: event.error ?? null },
        )
      }
      clearConnectionTestRun()
      void options.reload()
      return
    }
    if (event.type === 'error') {
      recordConnectionTestFailure(
        `failure-${event.source || 'unknown'}`,
        connectionTestFailureLabel(event),
        connectionTestFailureText(event),
        connectionTestFailureDiagnostics(event),
      )
      clearConnectionTestRun()
      void options.reload()
    }
  }

  function abortConnectionTest() {
    clearConnectionTestRun()
  }

  async function loadConnectionTestModels(account = testingAccount.value) {
    if (!account?.id)
      return
    loadingConnectionTestModels.value = true
    connectionTestError.value = ''
    try {
      const result = await getAccountModels({ accountId: account.id })
      applyConnectionTestModels(result)
      if (!connectionTestSelectedModel.value) {
        connectionTestError.value = '没有可测试模型'
      }
    }
    catch (error: unknown) {
      connectionTestError.value = errorMessage(error, '加载测试模型失败')
      connectionTestModelOptions.value = []
      connectionTestSelectedModel.value = ''
    }
    finally {
      loadingConnectionTestModels.value = false
    }
  }

  function applyConnectionTestModels(result: AccountModelsResponse, preserveSelection = false) {
    const previousSelection = preserveSelection ? connectionTestSelectedModel.value : ''
    connectionTestModelOptions.value = []
    for (const model of result.models ?? []) {
      connectionTestModelOptions.value.push({
        label: model.label || model.id,
        value: model.id,
      })
    }
    connectionTestSelectedModel.value = connectionTestModelOptions.value.some(
      model => model.value === previousSelection,
    )
      ? previousSelection
      : connectionTestModelOptions.value[0]?.value || ''
  }

  async function handleRefreshConnectionTestModels(account = testingAccount.value) {
    if (!account?.id || refreshingConnectionTestModels.value)
      return
    refreshingConnectionTestModels.value = true
    connectionTestError.value = ''
    try {
      const result = await refreshAccountModels({ accountId: account.id })
      applyConnectionTestModels(result, true)
      toast.success(`已刷新 ${connectionTestModelOptions.value.length} 个上游模型`)
    }
    catch (error: unknown) {
      connectionTestError.value = errorMessage(error, '刷新上游模型失败')
      toast.error(connectionTestError.value)
    }
    finally {
      refreshingConnectionTestModels.value = false
    }
  }

  async function handleTestConnection(account = testingAccount.value) {
    if (!account?.id)
      return
    if (!connectionTestSelectedModel.value) {
      connectionTestError.value = '请先选择测试模型'
      return
    }
    if (testingConnections.has(account.id))
      return
    abortConnectionTest()
    connectionTestStatus.value = 'running'
    connectionTestModel.value = ''
    connectionTestContent.value = ''
    connectionTestLogs.value = []
    connectionTestError.value = ''
    connectionTestDurationMs.value = null
    connectionTestModel.value = connectionTestSelectedModel.value
    connectionTestStartedAtMs = Date.now()
    connectionTestStartedAt.value = formatDateTime()
    connectionTestFinishedAt.value = ''
    appendConnectionTestLog('准备发送测试请求', 'info')
    testingConnections.add(account.id)
    try {
      await withMinimumDuration(
        () =>
          new Promise<void>((resolve) => {
            connectionTestRun = {
              accountId: account.id,
              resolve,
            }
            const params = new URLSearchParams({
              accountId: account.id,
              modelId: connectionTestSelectedModel.value,
            })
            connectionTestStreamUrl.value
              = `${API_BASE_URL}/api/admin/accounts/connection-test?${params}`
            openConnectionTestEventSource()
            if (!connectionTestEventSource.value)
              failConnectionTest('当前浏览器不支持连接测试')
          }),
      )
      if (connectionTestStatus.value === 'running') {
        recordConnectionTestFailure('failure', '测试失败', '测试连接未返回完成事件')
      }
    }
    catch (error: unknown) {
      recordConnectionTestFailure('failure', '测试失败', errorMessage(error, '测试连接失败'))
    }
    finally {
      clearConnectionTestRun()
    }
  }

  watch(connectionTestStreamMessage, (message) => {
    if (!message?.raw || !connectionTestRun)
      return
    try {
      const event = parseConnectionTestEvent(message.raw)
      if (event)
        handleConnectionTestEvent(event)
    }
    catch {
      failConnectionTest('测试响应解析失败')
    }
  })

  watch(connectionTestStreamError, (error) => {
    if (error && connectionTestRun)
      failConnectionTest('测试连接已断开')
  })

  watch(showConnectionTestModal, (open) => {
    if (!open) {
      abortConnectionTest()
    }
  })

  onBeforeUnmount(() => {
    abortConnectionTest()
  })

  return {
    showConnectionTestModal,
    testingAccount,
    connectionTestStatus,
    connectionTestModel,
    connectionTestLogs,
    connectionTestError,
    connectionTestStartedAt,
    connectionTestFinishedAt,
    connectionTestDurationMs,
    testingConnectionIds: testingConnections.ids,
    loadingConnectionTestModels,
    refreshingConnectionTestModels,
    connectionTestSelectedModel,
    connectionTestModelOptions,
    connectionTestStatusView,
    openConnectionTest,
    handleRefreshConnectionTestModels,
    handleTestConnection,
  }
}
