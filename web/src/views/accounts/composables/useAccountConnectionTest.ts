import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { CheckCircle2, Clock3, Wifi, XCircle } from '@lucide/vue'
import { clamp } from 'es-toolkit'

import { getAccountModels, testAccountConnectionStream } from '@/api'
import { withMinimumDuration } from '@/utils/async'
import { formatDateTime, formatTime } from '@/utils/date'

export function useAccountConnectionTest() {
  const showConnectionTestModal = ref(false)
  const testingAccount = ref<any>(null)
  const connectionTestStatus = ref('idle')
  const connectionTestModel = ref('')
  const connectionTestContent = ref('')
  const connectionTestLogs = ref<any[]>([])
  const connectionTestError = ref('')
  const connectionTestStartedAt = ref('')
  const connectionTestFinishedAt = ref('')
  const connectionTestDurationMs = ref<number | null>(null)
  const testingConnectionIds = ref<Set<string>>(new Set())
  const loadingConnectionTestModels = ref(false)
  const connectionTestSelectedModel = ref('')
  const connectionTestModelOptions = ref<any[]>([])

  let connectionTestAbortController: AbortController | undefined
  let connectionTestStartedAtMs = 0

  const connectionTestStatusView = computed(() => {
    if (connectionTestStatus.value === 'running') {
      return {
        label: '正在测试',
        description: '正在发送一条真实 Responses 流式请求。',
        icon: Clock3,
        badge: 'bg-(--cp-info-bg) text-(--cp-info-text)',
        iconClass: 'text-(--cp-info)',
      }
    }
    if (connectionTestStatus.value === 'success') {
      return {
        label: '连接正常',
        description: '账号令牌可用，已完成 Codex Responses 流式验证。',
        icon: CheckCircle2,
        badge: 'bg-(--cp-success-bg) text-(--cp-success-text)',
        iconClass: 'text-(--cp-success)',
      }
    }
    if (connectionTestStatus.value === 'error') {
      return {
        label: '测试失败',
        description: '真实请求未完成，优先检查令牌状态、账号权限或上游网络。',
        icon: XCircle,
        badge: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
        iconClass: 'text-(--cp-danger)',
      }
    }
    return {
      label: '准备测试',
      description: '点击开始后发送一条轻量 Responses 流式请求。',
      icon: Wifi,
      badge: 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)',
      iconClass: 'text-(--cp-text-muted)',
    }
  })

  function openConnectionTest(account: any) {
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

  function formatConnectionTestDetail(value: any) {
    if (value === undefined || value === null || value === '') return ''
    if (typeof value === 'string') return value
    return JSON.stringify(value, null, 2)
  }

  function connectionTestRequestText(payload: any) {
    const texts = (payload?.input || [])
      .flatMap((item: any) => item?.content || [])
      .filter((item: any) => item?.type === 'input_text' && item?.text)
      .map((item: any) => item.text)
    return texts.join('\n')
  }

  function connectionTestLogItem(key: string, text: string, tone = 'normal', detail?: any) {
    return {
      key,
      time: formatTime(),
      text,
      tone,
      detail: formatConnectionTestDetail(detail),
    }
  }

  function appendConnectionTestLog(text: string, tone = 'normal', detail?: any) {
    connectionTestLogs.value = [
      ...connectionTestLogs.value,
      connectionTestLogItem(`${Date.now()}-${connectionTestLogs.value.length}`, text, tone, detail),
    ]
  }

  function setConnectionTestLog(key: string, text: string, tone = 'normal', detail?: any) {
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

  function handleConnectionTestEvent(event: any) {
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
          setConnectionTestLog('response', '响应完成', 'success', '上游已完成，没有返回文本内容。')
        }
        appendConnectionTestLog('测试完成', 'success')
        finishConnectionTest('success')
      } else {
        connectionTestError.value = event.error || '测试连接失败'
        appendConnectionTestLog(connectionTestError.value, 'danger')
        finishConnectionTest('error')
      }
      return
    }
    if (event.type === 'error') {
      connectionTestError.value = event.error || '测试连接失败'
      appendConnectionTestLog(connectionTestError.value, 'danger')
      finishConnectionTest('error')
    }
  }

  function abortConnectionTest() {
    connectionTestAbortController?.abort()
    connectionTestAbortController = undefined
  }

  async function loadConnectionTestModels(account = testingAccount.value) {
    if (!account?.id) return
    loadingConnectionTestModels.value = true
    connectionTestError.value = ''
    try {
      const result = await getAccountModels({ id: account.id })
      connectionTestModelOptions.value = (result.models || []).map((model: any) => ({
        label: model.label || model.id,
        value: model.id,
      }))
      connectionTestSelectedModel.value = connectionTestModelOptions.value[0]?.value || ''
      if (!connectionTestSelectedModel.value) {
        connectionTestError.value = '没有可测试模型'
      }
    } catch (error: any) {
      connectionTestError.value = error.message || '加载测试模型失败'
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
    const controller = new AbortController()
    connectionTestAbortController = controller
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
      await withMinimumDuration(() =>
        testAccountConnectionStream(
          {
            id: account.id,
            modelId: connectionTestSelectedModel.value,
          },
          handleConnectionTestEvent,
          controller.signal,
        ),
      )
      if (connectionTestStatus.value === 'running') {
        connectionTestError.value = '测试连接未返回完成事件'
        appendConnectionTestLog(connectionTestError.value, 'danger')
        finishConnectionTest('error')
      }
    } catch (error: any) {
      if (error?.name !== 'AbortError') {
        connectionTestError.value = error.message || '测试连接失败'
        appendConnectionTestLog(connectionTestError.value, 'danger')
        finishConnectionTest('error')
      }
    } finally {
      const next = new Set(testingConnectionIds.value)
      next.delete(account.id)
      testingConnectionIds.value = next
      if (connectionTestAbortController === controller) {
        connectionTestAbortController = undefined
      }
    }
  }

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
