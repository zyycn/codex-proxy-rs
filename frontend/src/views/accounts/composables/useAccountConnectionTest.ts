import { computed, onBeforeUnmount, ref, shallowRef, watch } from 'vue'
import { CheckCircle2, Clock3, Wifi, XCircle } from '@lucide/vue'
import { useEventSource } from '@vueuse/core'
import { clamp } from 'es-toolkit'

import { getAccountModels } from '@/api'
import { API_BASE_URL } from '@/api/constants'
import { withMinimumDuration } from '@/utils/async'
import { formatDateTime, formatTime } from '@/utils/date'

interface ConnectionTestRun {
  accountId: string
  resolve: () => void
}

type ConnectionTestStatus = 'idle' | 'running' | 'success' | 'error'
type ConnectionTestLogTone = 'normal' | 'info' | 'success' | 'danger'

interface ConnectionTestAccount {
  id: string
  status: string
  displayStatus?: string
  tokenRefreshing?: boolean
}

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
  input?: Array<{
    content?: Array<{
      type?: string
      text?: string
    }>
  }>
}

type ConnectionTestEvent =
  | { type: 'test_start'; model?: string }
  | { type: 'request'; payload?: ConnectionTestRequestPayload }
  | { type: 'status'; text?: string }
  | { type: 'content'; text?: string }
  | { type: 'test_complete'; success?: boolean; error?: string; accountStatus?: string }
  | { type: 'error'; error?: string; accountStatus?: string }

interface AccountModelsResponse {
  models?: Array<{
    id: string
    label?: string | null
  }>
}

interface AccountConnectionTestOptions {
  onAccountStatus: (accountId: string, status: string) => void
}

export function useAccountConnectionTest(options: AccountConnectionTestOptions) {
  const showConnectionTestModal = shallowRef(false)
  const testingAccount = shallowRef<ConnectionTestAccount | null>(null)
  const connectionTestStatus = shallowRef<ConnectionTestStatus>('idle')
  const connectionTestModel = shallowRef('')
  const connectionTestContent = shallowRef('')
  const connectionTestLogs = ref<ConnectionTestLog[]>([])
  const connectionTestError = shallowRef('')
  const connectionTestStartedAt = shallowRef('')
  const connectionTestFinishedAt = shallowRef('')
  const connectionTestDurationMs = shallowRef<number | null>(null)
  const testingConnectionIds = ref<Set<string>>(new Set())
  const loadingConnectionTestModels = shallowRef(false)
  const connectionTestSelectedModel = shallowRef('')
  const connectionTestModelOptions = ref<ConnectionTestModelOption[]>([])
  const connectionTestUrl = shallowRef<string | undefined>()

  let connectionTestStartedAtMs = 0
  let connectionTestRun: ConnectionTestRun | undefined

  const {
    data: connectionTestStreamEvent,
    error: connectionTestStreamError,
    status: connectionTestStreamStatus,
    close: closeConnectionTestStream,
    open: openConnectionTestStream,
  } = useEventSource<[], ConnectionTestEvent | null>(connectionTestUrl, [], {
    immediate: false,
    autoConnect: false,
    serializer: {
      read: (raw?: string) => (raw ? (JSON.parse(raw) as ConnectionTestEvent) : null),
    },
  })

  const connectionTestStatusView = computed(() => {
    if (connectionTestStatus.value === 'running') {
      return {
        label: '正在测试',
        description: '正在发送一条真实 Responses 流式请求',
        icon: Clock3,
        badge: 'bg-(--cp-info-bg) text-(--cp-info-text)',
        iconClass: 'text-(--cp-info)',
      }
    }
    if (connectionTestStatus.value === 'success') {
      return {
        label: '连接正常',
        description: '账号令牌可用，已完成 Codex Responses 流式验证',
        icon: CheckCircle2,
        badge: 'bg-(--cp-success-bg) text-(--cp-success-text)',
        iconClass: 'text-(--cp-success)',
      }
    }
    if (connectionTestStatus.value === 'error') {
      return {
        label: '测试失败',
        description: '真实请求未完成，优先检查令牌状态、账号权限或上游网络',
        icon: XCircle,
        badge: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
        iconClass: 'text-(--cp-danger)',
      }
    }
    return {
      label: '准备测试',
      description: '点击开始后发送一条轻量 Responses 流式请求',
      icon: Wifi,
      badge: 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)',
      iconClass: 'text-(--cp-text-muted)',
    }
  })

  function openConnectionTest(account: ConnectionTestAccount) {
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
    if (value === undefined || value === null || value === '') return ''
    if (typeof value === 'string') return value
    return JSON.stringify(value, null, 2)
  }

  function connectionTestRequestText(payload?: ConnectionTestRequestPayload) {
    const texts = (payload?.input ?? [])
      .flatMap((item) => item.content ?? [])
      .filter((item) => item.type === 'input_text' && item.text)
      .map((item) => item.text)
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
    const index = connectionTestLogs.value.findIndex((item) => item.key === key)
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

  function applyAccountStatus(status?: string) {
    const account = testingAccount.value
    if (!account || !status) return
    testingAccount.value = {
      ...account,
      status,
      displayStatus: account.tokenRefreshing ? account.displayStatus : status,
    }
    options.onAccountStatus(account.id, status)
  }

  function clearConnectionTestRun() {
    const run = connectionTestRun
    connectionTestRun = undefined
    closeConnectionTestStream()
    connectionTestUrl.value = undefined
    if (run) {
      const next = new Set(testingConnectionIds.value)
      next.delete(run.accountId)
      testingConnectionIds.value = next
      run.resolve()
    }
  }

  function failConnectionTest(message = '测试连接失败') {
    if (connectionTestStatus.value === 'running') {
      connectionTestError.value = message
      appendConnectionTestLog(connectionTestError.value, 'danger')
      finishConnectionTest('error')
    }
    clearConnectionTestRun()
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
      applyAccountStatus(event.accountStatus)
      if (event.success) {
        if (!connectionTestContent.value) {
          setConnectionTestLog('response', '响应完成', 'success', '上游已完成，没有返回文本内容')
        }
        appendConnectionTestLog('测试完成', 'success')
        finishConnectionTest('success')
      } else {
        connectionTestError.value = event.error || '测试连接失败'
        appendConnectionTestLog(connectionTestError.value, 'danger')
        finishConnectionTest('error')
      }
      clearConnectionTestRun()
      return
    }
    if (event.type === 'error') {
      applyAccountStatus(event.accountStatus)
      connectionTestError.value = event.error || '测试连接失败'
      appendConnectionTestLog(connectionTestError.value, 'danger')
      finishConnectionTest('error')
      clearConnectionTestRun()
    }
  }

  function abortConnectionTest() {
    clearConnectionTestRun()
  }

  function connectionTestStreamUrl(accountId: string, modelId: string) {
    const params = new URLSearchParams({
      id: accountId,
      modelId,
    })
    return `${API_BASE_URL}/api/admin/accounts/test?${params}`
  }

  function errorMessage(error: unknown, fallback: string) {
    return error instanceof Error ? error.message : fallback
  }

  async function loadConnectionTestModels(account = testingAccount.value) {
    if (!account?.id) return
    loadingConnectionTestModels.value = true
    connectionTestError.value = ''
    try {
      const result = (await getAccountModels({ id: account.id })) as AccountModelsResponse
      connectionTestModelOptions.value = (result.models ?? []).map((model) => ({
        label: model.label || model.id,
        value: model.id,
      }))
      connectionTestSelectedModel.value = connectionTestModelOptions.value[0]?.value || ''
      if (!connectionTestSelectedModel.value) {
        connectionTestError.value = '没有可测试模型'
      }
    } catch (error: unknown) {
      connectionTestError.value = errorMessage(error, '加载测试模型失败')
      connectionTestModelOptions.value = []
      connectionTestSelectedModel.value = ''
    } finally {
      loadingConnectionTestModels.value = false
    }
  }

  async function handleTestConnection(account = testingAccount.value) {
    if (!account?.id) return
    if (!connectionTestSelectedModel.value) {
      connectionTestError.value = '请先选择测试模型'
      return
    }
    if (testingConnectionIds.value.has(account.id)) return
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
    testingConnectionIds.value = new Set(testingConnectionIds.value).add(account.id)
    try {
      await withMinimumDuration(
        () =>
          new Promise<void>((resolve) => {
            connectionTestRun = {
              accountId: account.id,
              resolve,
            }
            connectionTestUrl.value = connectionTestStreamUrl(
              account.id,
              connectionTestSelectedModel.value,
            )
            openConnectionTestStream()
          }),
      )
      if (connectionTestStatus.value === 'running') {
        connectionTestError.value = '测试连接未返回完成事件'
        appendConnectionTestLog(connectionTestError.value, 'danger')
        finishConnectionTest('error')
      }
    } catch (error: unknown) {
      connectionTestError.value = errorMessage(error, '测试连接失败')
      appendConnectionTestLog(connectionTestError.value, 'danger')
      finishConnectionTest('error')
    } finally {
      clearConnectionTestRun()
    }
  }

  watch(connectionTestStreamEvent, (event) => {
    if (event) {
      handleConnectionTestEvent(event)
    }
  })

  watch(connectionTestStreamError, (event) => {
    if (event && connectionTestRun) {
      failConnectionTest('测试连接已断开')
    }
  })

  watch(connectionTestStreamStatus, (status) => {
    if (status === 'CLOSED' && connectionTestRun && connectionTestStatus.value === 'running') {
      failConnectionTest('测试连接已关闭')
    }
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
    testingConnectionIds,
    loadingConnectionTestModels,
    connectionTestSelectedModel,
    connectionTestModelOptions,
    connectionTestStatusView,
    openConnectionTest,
    handleTestConnection,
  }
}
