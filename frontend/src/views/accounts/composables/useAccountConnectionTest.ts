import type { Account, AccountModelsResponse } from '@/api'
import { CheckCircle2, Clock3, Wifi, XCircle } from '@lucide/vue'
import { useEventSource } from '@vueuse/core'

import { clamp } from 'es-toolkit'
import { computed, onBeforeUnmount, ref, shallowRef, watch } from 'vue'
import { getAccountModels, refreshAccountModels } from '@/api'
import { API_BASE_URL } from '@/api/constants'
import { useIdSet } from '@/composables/useIdSet'
import { useRequestState } from '@/composables/useRequestState'
import { t } from '@/i18n'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage, withMinimumDuration } from '@/utils/operation'

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
  failureMessage?: string
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
  proxyHint?: string | null
  sendState?: ConnectionTestSendState | null
  error?: string
  providerErrorCode?: string | null
  providerErrorType?: string | null
  upstreamStatus?: number | null
  upstreamContentType?: string | null
  upstreamBody?: string | null
}

interface ConnectionTestTimestamp { occurredAtDisplay: string, timeDisplay: string }

type ConnectionTestEvent = ConnectionTestTimestamp & (
  | ConnectionTestStartEvent
  | ConnectionTestRequestEvent
  | ConnectionTestStatusEvent
  | ConnectionTestContentEvent
  | ConnectionTestCompleteEvent
  | ConnectionTestFailureEvent
)

const CONNECTION_TEST_EVENT_TYPES = new Set<ConnectionTestEvent['type']>([
  'test_start',
  'request',
  'status',
  'content',
  'test_complete',
  'error',
])

const CONNECTION_TEST_FAILURE_TEXT = computed<Record<string, string>>(() => ({
  invalid_request: t('accounts.connectionTest.errors.invalidRequest'),
  unsupported: t('accounts.connectionTest.errors.unsupported'),
  unauthorized: t('accounts.connectionTest.errors.credentials'),
  policy_denied: t('accounts.connectionTest.errors.policyRejected'),
  model_not_found: t('accounts.connectionTest.errors.modelNotFound'),
  no_available_provider: t('accounts.connectionTest.errors.accountUnavailable'),
  account_capacity_unavailable: t('accounts.connectionTest.errors.capacityUnavailable'),
  provider_infrastructure_unavailable: t('accounts.connectionTest.errors.infrastructureUnavailable'),
  rate_limited: t('accounts.connectionTest.errors.rateLimited'),
  upstream_unavailable: t('accounts.connectionTest.errors.upstreamUnavailable'),
  timeout: t('accounts.connectionTest.errors.timeout'),
  cancelled: t('accounts.connectionTest.errors.canceled'),
  internal_error: t('accounts.connectionTest.errors.internal'),
}))

const CONNECTION_TEST_SOURCE_LABEL = computed<Record<ConnectionTestFailureSource, string>>(() => ({
  gateway: t('accounts.connectionTest.stages.gateway'),
  provider: t('accounts.connectionTest.stages.provider'),
  upstream: t('accounts.connectionTest.stages.upstream'),
}))

function parseConnectionTestEvent(raw: string): ConnectionTestEvent | null {
  const value: unknown = JSON.parse(raw)
  if (!value || typeof value !== 'object' || !('type' in value) || typeof value.type !== 'string')
    throw new TypeError('invalid connection-test event')
  if (!CONNECTION_TEST_EVENT_TYPES.has(value.type as ConnectionTestEvent['type']))
    return null
  return value as ConnectionTestEvent
}

function connectionTestFailureText(event: ConnectionTestFailureEvent) {
  if (event.source === 'provider' && event.sendState === 'not_sent') {
    switch (event.proxyHint) {
      case 'invalid_configuration': return t('accounts.connectionTest.errors.proxyInvalid')
      case 'unreachable': return t('accounts.connectionTest.errors.proxyUnreachable')
      case 'tunnel_failed': return t('accounts.connectionTest.errors.proxyTunnelFailed')
      case 'check_proxy': return t('accounts.connectionTest.errors.checkProxy')
    }
    if (event.gatewayErrorCode === 'upstream_unavailable')
      return t('accounts.connectionTest.errors.notSent')
  }
  return event.gatewayErrorCode
    ? CONNECTION_TEST_FAILURE_TEXT.value[event.gatewayErrorCode] || t('accounts.connectionTest.errors.unknown')
    : t('accounts.connectionTest.errors.failed')
}

function connectionTestFailureLabel(event: ConnectionTestFailureEvent) {
  return event.source
    ? CONNECTION_TEST_SOURCE_LABEL.value[event.source] || t('accounts.connectionTest.failed')
    : t('accounts.connectionTest.failed')
}

function connectionTestFailureDiagnostics(event: ConnectionTestFailureEvent) {
  return {
    error: event.error ?? null,
    gatewayErrorCode: event.gatewayErrorCode ?? null,
    proxyHint: event.proxyHint ?? null,
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
  const modelsRequest = useRequestState()
  const modelsRequestMode = shallowRef<'load' | 'refresh'>('load')
  const loadingConnectionTestModels = computed(() => modelsRequest.loading.value && modelsRequestMode.value === 'load')
  const refreshingConnectionTestModels = computed(() => modelsRequest.loading.value && modelsRequestMode.value === 'refresh')
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
        label: t('accounts.connectionTest.testing'),
        description: t('accounts.connectionTest.runningDescription'),
        icon: Clock3,
        badge: 'bg-z-info-container text-z-info-on-container',
        iconClass: 'text-z-info',
      }
    }
    if (connectionTestStatus.value === 'success') {
      return {
        label: t('accounts.connectionTest.successTitle'),
        description: t('accounts.connectionTest.completedDescription'),
        icon: CheckCircle2,
        badge: 'bg-z-success-container text-z-success-on-container',
        iconClass: 'text-z-success',
      }
    }
    if (connectionTestStatus.value === 'error') {
      return {
        label: t('accounts.connectionTest.failed'),
        description: connectionTestLogs.value.at(-1)?.failureMessage,
        icon: XCircle,
        badge: 'bg-z-error-container text-z-error-on-container',
        iconClass: 'text-z-error',
      }
    }
    return {
      label: t('accounts.connectionTest.readyTitle'),
      description: t('accounts.connectionTest.readyDescription'),
      icon: Wifi,
      badge: 'bg-z-fill-quaternary text-z-text-secondary',
      iconClass: 'text-z-text-quaternary',
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
    event?: ConnectionTestTimestamp,
  ): ConnectionTestLog {
    return {
      key,
      time: event?.timeDisplay ?? '',
      text,
      tone,
      detail: formatConnectionTestDetail(detail),
    }
  }

  function appendConnectionTestLog(
    text: string,
    tone: ConnectionTestLogTone = 'normal',
    detail?: unknown,
    event?: ConnectionTestTimestamp,
  ) {
    connectionTestLogs.value = [
      ...connectionTestLogs.value,
      connectionTestLogItem(`${Date.now()}-${connectionTestLogs.value.length}`, text, tone, detail, event),
    ]
  }

  function setConnectionTestLog(
    key: string,
    text: string,
    tone: ConnectionTestLogTone = 'normal',
    detail?: unknown,
    event?: ConnectionTestTimestamp,
  ) {
    const index = connectionTestLogs.value.findIndex(item => item.key === key)
    const next = connectionTestLogItem(key, text, tone, detail, event)
    if (index === -1) {
      connectionTestLogs.value = [...connectionTestLogs.value, next]
      return
    }
    connectionTestLogs.value = connectionTestLogs.value.map((item, itemIndex) =>
      itemIndex === index
        ? { ...next, time: item.time }
        : item,
    )
  }

  function finishConnectionTest(status: 'success' | 'error', event?: ConnectionTestTimestamp) {
    connectionTestStatus.value = status
    connectionTestFinishedAt.value = event?.occurredAtDisplay ?? ''
    connectionTestDurationMs.value = clamp(
      performance.now() - connectionTestStartedAtMs,
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

  function failConnectionTest(message = t('accounts.connectionTest.errors.failed')) {
    if (connectionTestStatus.value === 'running') {
      recordConnectionTestFailure('failure', t('accounts.connectionTest.failed'), message)
    }
    clearConnectionTestRun()
  }

  function recordConnectionTestFailure(
    key: string,
    label: string,
    message: string,
    detail?: unknown,
    event?: ConnectionTestTimestamp,
  ) {
    connectionTestLogs.value.push({
      ...connectionTestLogItem(key, label, 'danger', detail, event),
      failureMessage: message,
    })
    finishConnectionTest('error', event)
  }

  function handleConnectionTestEvent(event: ConnectionTestEvent) {
    if (event.type === 'test_start') {
      connectionTestStartedAt.value = event.occurredAtDisplay
      connectionTestModel.value = event.model || connectionTestModel.value
      appendConnectionTestLog(t('accounts.connectionTest.starting', { value: connectionTestModel.value || t('accounts.connectionTest.errors.modelRequired') }), 'info', undefined, event)
      return
    }
    if (event.type === 'request') {
      setConnectionTestLog('request', t('accounts.connectionTest.events.request'), 'info', connectionTestRequestText(event.payload), event)
      return
    }
    if (event.type === 'status' && event.text) {
      appendConnectionTestLog(event.text, 'info', undefined, event)
      return
    }
    if (event.type === 'content' && event.text) {
      connectionTestContent.value += event.text
      setConnectionTestLog('response', t('accounts.connectionTest.events.response'), 'success', connectionTestContent.value, event)
      return
    }
    if (event.type === 'test_complete') {
      if (event.success) {
        if (!connectionTestContent.value) {
          setConnectionTestLog('response', t('accounts.connectionTest.events.completed'), 'success', t('accounts.connectionTest.emptyResponse'), event)
        }
        appendConnectionTestLog(t('accounts.connectionTest.testCompleted'), 'success', undefined, event)
        finishConnectionTest('success', event)
      }
      else {
        recordConnectionTestFailure(
          'test-complete-failure',
          t('accounts.connectionTest.failed'),
          t('accounts.connectionTest.errors.failed'),
          { error: event.error ?? null },
          event,
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
        event,
      )
      clearConnectionTestRun()
      void options.reload()
    }
  }

  function abortConnectionTest() {
    clearConnectionTestRun()
  }

  async function loadConnectionTestModels(account = testingAccount.value, refresh = false) {
    if (!account?.id || !showConnectionTestModal.value || account.id !== testingAccount.value?.id)
      return
    // 初次查询与主动刷新属于同一弹窗会话，旧请求不得覆盖新账号或新一轮查询。
    const requestId = modelsRequest.start()
    modelsRequestMode.value = refresh ? 'refresh' : 'load'
    connectionTestError.value = ''
    try {
      const result = await (refresh ? refreshAccountModels : getAccountModels)(
        { accountId: account.id },
        { signal: modelsRequest.signal },
      )
      if (!modelsRequest.isCurrent(requestId))
        return
      applyConnectionTestModels(result.models, account, refresh)
      if (refresh) {
        ZNotification.success({
          message: t('accounts.connectionTest.modelsRefreshed', { count: result.models.length }),
        })
      }
      else if (!connectionTestSelectedModel.value) {
        connectionTestError.value = t('accounts.connectionTest.noModelsAvailable')
      }
    }
    catch (error: unknown) {
      if (!modelsRequest.isCurrent(requestId))
        return
      connectionTestError.value = errorMessage(error)
      if (!refresh)
        applyConnectionTestModels([], account)
    }
    finally {
      modelsRequest.finish(requestId)
    }
  }

  function applyConnectionTestModels(catalog: AccountModelsResponse['models'], account: Account, preserveSelection = false) {
    const previousSelection = preserveSelection ? connectionTestSelectedModel.value : ''
    const labels = new Map(catalog.map(model => [model.id, model.label || model.id]))
    if (account.modelAccess.mode === 'allowlist') {
      for (const id of account.modelAccess.models) {
        if (!labels.has(id))
          labels.set(id, id)
      }
    }
    connectionTestModelOptions.value = Array.from(labels, ([value, label]) => ({ value, label }))
    connectionTestSelectedModel.value = labels.has(previousSelection)
      ? previousSelection
      : connectionTestModelOptions.value[0]?.value || ''
  }

  async function handleRefreshConnectionTestModels(account = testingAccount.value) {
    if (modelsRequest.loading.value)
      return
    await loadConnectionTestModels(account, true)
  }

  async function handleTestConnection(account = testingAccount.value) {
    if (!account?.id)
      return
    if (!connectionTestSelectedModel.value) {
      connectionTestError.value = t('accounts.connectionTest.errors.modelRequired')
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
    connectionTestStartedAtMs = performance.now()
    connectionTestStartedAt.value = ''
    connectionTestFinishedAt.value = ''
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
              failConnectionTest(t('accounts.connectionTest.errors.browserUnsupported'))
          }),
      )
      if (connectionTestStatus.value === 'running') {
        recordConnectionTestFailure('failure', t('accounts.connectionTest.failed'), t('accounts.connectionTest.errors.missingCompletion'))
      }
    }
    catch (error: unknown) {
      recordConnectionTestFailure('failure', t('accounts.connectionTest.failed'), errorMessage(error, t('accounts.connectionTest.errors.failed')))
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
      failConnectionTest(t('accounts.connectionTest.errors.invalidResponse'))
    }
  })

  watch(connectionTestStreamError, (error) => {
    if (error && connectionTestRun)
      failConnectionTest(t('accounts.connectionTest.errors.disconnected'))
  })

  watch([
    showConnectionTestModal,
    () => testingAccount.value?.id,
  ], ([open]) => {
    modelsRequest.invalidate({ resetLoading: open })
    if (!open) {
      abortConnectionTest()
    }
  }, { flush: 'sync' })

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
