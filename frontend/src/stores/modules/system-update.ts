import type { SystemUpdateChannel, SystemUpdatePolicy, SystemUpdateStatus } from '@/api'
import { until, useEventSource, useTimeoutPoll } from '@vueuse/core'
import { delay } from 'es-toolkit'
import { defineStore } from 'pinia'

import { computed, ref, shallowRef, watch } from 'vue'
import {
  getSystemUpdateDetail,
  getSystemUpdateStatus,
  getSystemVersion,
  performSystemUpdate,
  restartSystem,
} from '@/api'
import { API_BASE_URL } from '@/api/constants'
import { ApiError } from '@/api/request'
import { errorMessage } from '@/utils/operation'

const maxUpdateLogs = 200
const updateEventReadyTimeoutMs = 3_000
const restartReadyTimeoutMs = 60_000
const restartProbeTimeoutMs = 2_000
const restartReadyPollIntervalMs = 500
const updateStatusPollIntervalMs = 1_000
const updateStatusTimeoutMs = 5_000

interface SystemUpdateEvent {
  id: string
  operationId?: string | null
  level: string
  message: string
  at: string
  step?: string
  terminal?: boolean
}

// 单一更新阶段状态机：互斥的操作态无法被类型同时表达。
type SystemUpdatePhase
  = | { kind: 'idle' }
    | { kind: 'loading' }
    | { kind: 'checking' }
    | { kind: 'changing_channel' }
    | { kind: 'ready' }
    | { kind: 'updating' }
    | { kind: 'restart_required' }
    | { kind: 'restarting' }
    | { kind: 'failed' }

const UPDATE_BUSY_PHASES = new Set<SystemUpdatePhase['kind']>([
  'loading',
  'checking',
  'changing_channel',
  'updating',
  'restarting',
])

export const useSystemUpdateStore = defineStore('system-update', () => {
  const version = shallowRef<Awaited<ReturnType<typeof getSystemVersion>> | null>(null)
  const updateInfo = shallowRef<Awaited<ReturnType<typeof getSystemUpdateDetail>> | null>(null)
  const phase = shallowRef<SystemUpdatePhase>({ kind: 'idle' })
  const updateError = shallowRef('')
  const statusAvailable = shallowRef(false)
  const policy = shallowRef<SystemUpdatePolicy | null>(null)
  const loadedOnce = shallowRef(false)
  const updateLogs = ref<SystemUpdateEvent[]>([])
  const updateStreaming = shallowRef(false)
  const updateStreamError = shallowRef('')
  const updateStatus = shallowRef<SystemUpdateStatus | null>(null)
  const {
    data: updateEventMessage,
    status: updateEventStatus,
    error: updateEventError,
    eventSource: updateEventSource,
    open: openUpdateEventSource,
    close: closeUpdateEventSource,
  } = useEventSource(`${API_BASE_URL}/api/admin/system/update/events`, ['update'], {
    autoConnect: false,
    immediate: false,
    withCredentials: true,
    serializer: {
      read: raw => ({ raw }),
    },
  })

  let loadVersionPromise: ReturnType<typeof getSystemVersion> | undefined
  let loadSystemPromise: Promise<void> | undefined
  let statusRequest: Promise<void> | undefined
  let statusGeneration = 0
  let detailGeneration = 0
  let submitting = false
  let activeOperationId: string | null = null
  let unconfirmedPreviousId: string | null | undefined
  const statusPoll = useTimeoutPoll(refreshUpdateStatus, updateStatusPollIntervalMs, { immediate: false })

  const phaseKind = computed(() => phase.value.kind)
  const loading = computed(() => phaseKind.value === 'loading')
  const checking = computed(() => phaseKind.value === 'checking')
  const changingChannel = computed(() => phaseKind.value === 'changing_channel')
  const updating = computed(() => phaseKind.value === 'updating')
  const restarting = computed(() => phaseKind.value === 'restarting')
  const lastFailedOperation = computed(() => {
    const operation = updateStatus.value?.operation
    return operation?.status === 'failed' && !updateError.value && !UPDATE_BUSY_PHASES.has(phaseKind.value)
      ? operation
      : null
  })

  const needRestart = computed(() => Boolean(updateStatus.value?.needRestart))
  const restartTargetVersion = computed(() => needRestart.value ? normalizeSystemVersion(updateStatus.value?.currentVersion) : '')
  const selectedChannel = computed(() => policy.value?.channel ?? 'stable')
  const availableChannels = computed(() => policy.value?.availableChannels ?? [])
  const canChangeChannel = computed(() => availableChannels.value.length > 1
    && Boolean(updateInfo.value) && !updateInfo.value?.unsupportedReason
    && statusAvailable.value && !needRestart.value && !UPDATE_BUSY_PHASES.has(phaseKind.value))
  const hasUpdate = computed(() => Boolean(version.value?.hasUpdate))
  const hasCandidateUpdate = computed(() => Boolean(updateInfo.value?.hasUpdate))
  const canUpdate = computed(
    () =>
      hasCandidateUpdate.value
      && statusAvailable.value
      && updateInfo.value?.policy.channel === selectedChannel.value
      && Boolean(updateInfo.value?.updateSupported)
      && !needRestart.value
      && !UPDATE_BUSY_PHASES.has(phaseKind.value),
  )

  function setPhase(next: SystemUpdatePhase) {
    phase.value = next
  }

  function resetUpdateResult() {
    updateError.value = ''
    activeOperationId = null
  }

  function appendUpdateLog(log: SystemUpdateEvent) {
    const logs = updateLogs.value.filter(item => item.id !== log.id)
    updateLogs.value = [...logs, log].slice(-maxUpdateLogs)
  }

  function clearUpdateLogs() {
    updateLogs.value = []
    updateStreamError.value = ''
  }

  watch(updateEventStatus, (status) => {
    updateStreaming.value = status === 'OPEN'
    if (status === 'OPEN')
      updateStreamError.value = ''
  })

  watch(updateEventError, (error) => {
    if (error)
      updateStreamError.value = '更新日志连接中断'
  })

  watch(updateEventMessage, (message) => {
    if (!message?.raw)
      return
    try {
      const event = JSON.parse(message.raw) as SystemUpdateEvent
      if (activeOperationId && event.operationId !== activeOperationId)
        return
      appendUpdateLog(event)
      if (event.terminal && !submitting)
        void refreshUpdateStatus()
    }
    catch {
      updateStreamError.value = '更新日志解析失败'
    }
  })

  async function connectUpdateEvents(force = false) {
    if (force) {
      disconnectUpdateEvents()
    }
    else if (updateEventSource.value) {
      return updateEventStatus.value === 'OPEN'
    }

    updateStreamError.value = ''
    openUpdateEventSource()
    if (!updateEventSource.value) {
      updateStreamError.value = '当前浏览器不支持实时更新日志'
      return false
    }

    if (updateEventStatus.value !== 'OPEN') {
      await until(updateEventStatus).toBe('OPEN', {
        timeout: updateEventReadyTimeoutMs,
      })
    }
    const connected = updateEventStatus.value === 'OPEN'
    if (!connected)
      updateStreamError.value = '更新日志连接超时'
    return connected
  }

  function disconnectUpdateEvents() {
    closeUpdateEventSource()
  }

  function applyUpdateStatus(status: SystemUpdateStatus) {
    const operation = status.operation
    const confirmingOperation = unconfirmedPreviousId !== undefined
    statusAvailable.value = true
    updateStatus.value = status
    if (unconfirmedPreviousId !== undefined && operation.operationId === unconfirmedPreviousId) {
      unconfirmedPreviousId = undefined
      updateError.value = '尚未确认更新任务，请检查状态后重试'
      setPhase({ kind: 'ready' })
      statusPoll.pause()
      disconnectUpdateEvents()
      return
    }
    unconfirmedPreviousId = undefined
    // 只跟踪本次提交或仍在运行的任务，持久化终态属于历史，不代表当前安装异常。
    if (operation.status === 'running' || confirmingOperation)
      activeOperationId = operation.operationId
    updateError.value = ''
    if (operation.status === 'running') {
      setPhase({ kind: 'updating' })
      if (!statusPoll.isActive.value)
        statusPoll.resume()
      void connectUpdateEvents()
      return
    }
    statusPoll.pause()
    disconnectUpdateEvents()
    if (operation.status === 'failed' && activeOperationId && operation.operationId === activeOperationId) {
      updateError.value = operation.error || operation.message || '更新失败'
    }
    if (!['loading', 'checking', 'changing_channel'].includes(phaseKind.value))
      settlePhase()
  }

  async function refreshUpdateStatus() {
    if (submitting || restarting.value)
      return
    if (statusRequest)
      return statusRequest
    const generation = statusGeneration
    statusRequest = (async () => {
      try {
        const status = await getSystemUpdateStatus({ silent: true, timeout: updateStatusTimeoutMs })
        if (generation !== statusGeneration)
          return
        updateStreamError.value = ''
        applyUpdateStatus(status)
      }
      catch (error: unknown) {
        if (generation !== statusGeneration)
          return
        statusAvailable.value = false
        const nonRetryable = error instanceof ApiError && error.status >= 400 && error.status < 500 && error.status !== 408 && error.status !== 429
        if (nonRetryable || (!updating.value && unconfirmedPreviousId === undefined)) {
          statusPoll.pause()
          disconnectUpdateEvents()
          updateError.value = errorMessage(error)
          setPhase({ kind: 'failed' })
          return
        }
        // 传输失败不能证明任务失败，保留忙碌态，恢复连接后重新核对持久化结果。
        updateStreamError.value = '暂时无法确认更新状态，正在重试'
        setPhase({ kind: 'updating' })
        if (!statusPoll.isActive.value)
          statusPoll.resume()
      }
      finally {
        statusRequest = undefined
      }
    })()
    return statusRequest
  }

  function settlePhase() {
    setPhase(needRestart.value ? { kind: 'restart_required' } : updateError.value ? { kind: 'failed' } : { kind: 'ready' })
  }

  async function loadDetail(refresh: boolean, generation: number, channel?: SystemUpdateChannel) {
    const detail = await getSystemUpdateDetail({ refresh, channel })
    if (generation !== detailGeneration)
      return
    updateInfo.value = detail
    policy.value = detail.policy
    loadedOnce.value = true
    if (!version.value)
      await loadVersion()
    if (generation !== detailGeneration)
      return
    if (version.value && detail.policy.channel === version.value.updateChannel) {
      // 侧栏只提示当前运行通道的更新，临时查看其他通道不会改变全局提示。
      version.value = { ...version.value, latestVersion: detail.latestVersion, hasUpdate: detail.hasUpdate, updateCached: detail.cached, updateWarning: detail.warning }
    }
  }

  async function loadSystem(refresh = false) {
    if (loadSystemPromise)
      return loadSystemPromise
    if (updating.value || restarting.value)
      return

    const generation = ++detailGeneration
    resetUpdateResult()
    setPhase({ kind: 'loading' })
    loadSystemPromise = (async () => {
      await Promise.all([
        refreshUpdateStatus(),
        loadDetail(refresh, generation),
      ])
    })()

    try {
      await loadSystemPromise
    }
    finally {
      if (phaseKind.value === 'loading')
        settlePhase()
      loadSystemPromise = undefined
    }
  }

  async function loadVersion() {
    if (loadVersionPromise)
      return loadVersionPromise

    loadVersionPromise = getSystemVersion({ silent: true })
    try {
      const versionData = await loadVersionPromise
      version.value = versionData
      return versionData
    }
    finally {
      loadVersionPromise = undefined
    }
  }

  async function checkUpdates(refresh = true) {
    if (UPDATE_BUSY_PHASES.has(phaseKind.value))
      return updateInfo.value

    const generation = ++detailGeneration
    setPhase({ kind: 'checking' })
    resetUpdateResult()
    try {
      await loadDetail(refresh, generation, selectedChannel.value)
      if (generation !== detailGeneration)
        return
      await refreshUpdateStatus()
      return updateInfo.value
    }
    finally {
      if (generation === detailGeneration && phaseKind.value === 'checking')
        settlePhase()
    }
  }

  async function changeChannel(channel: SystemUpdateChannel) {
    if (!canChangeChannel.value || channel === selectedChannel.value)
      return
    const generation = ++detailGeneration
    setPhase({ kind: 'changing_channel' })
    resetUpdateResult()
    try {
      if (policy.value)
        policy.value = { ...policy.value, channel }
      // 选择只影响本次检查，重新打开时恢复运行通道；旧候选不能用于新通道下载。
      await loadDetail(true, generation, channel)
      if (generation !== detailGeneration)
        return
      await refreshUpdateStatus()
    }
    catch (error: unknown) {
      if (generation !== detailGeneration)
        return
      if (updateInfo.value?.policy.channel !== selectedChannel.value)
        updateInfo.value = null
      updateError.value = errorMessage(error)
      throw error
    }
    finally {
      if (generation === detailGeneration && phaseKind.value === 'changing_channel')
        settlePhase()
    }
  }

  async function updateNow(targetVersion: string, channel: SystemUpdateChannel) {
    const confirmedTargetVersion = normalizeSystemVersion(targetVersion)
    if (!canUpdate.value || !confirmedTargetVersion || channel !== selectedChannel.value
      || confirmedTargetVersion !== normalizeSystemVersion(updateInfo.value?.latestVersion)) {
      return null
    }

    statusGeneration += 1
    submitting = true
    statusPoll.pause()
    unconfirmedPreviousId = undefined
    const previousId = updateStatus.value?.operation.operationId ?? null
    resetUpdateResult()
    clearUpdateLogs()
    setPhase({ kind: 'updating' })
    try {
      await connectUpdateEvents(true)
      const result = await performSystemUpdate({ targetVersion: confirmedTargetVersion, channel }, { silent: true })
      activeOperationId = result.operationId
      updateLogs.value = updateLogs.value.filter(event => event.operationId === result.operationId)
      return result
    }
    catch (error: unknown) {
      if (error instanceof ApiError && (error.status === 0 || error.status >= 500 || error.status === 408)) {
        unconfirmedPreviousId = previousId
        updateStreamError.value = '正在确认更新是否已开始'
        return null
      }
      updateError.value = errorMessage(error)
      setPhase({ kind: 'failed' })
      disconnectUpdateEvents()
      throw error
    }
    finally {
      submitting = false
      if (updating.value) {
        statusPoll.resume()
        // 提交前的查询可能仍在返回，先丢弃旧代次结果，再读取本次任务。
        await statusRequest
        await refreshUpdateStatus()
      }
    }
  }

  async function waitForServiceAndReload() {
    const expectedVersion = restartTargetVersion.value
    const deadline = Date.now() + restartReadyTimeoutMs

    while (Date.now() < deadline) {
      try {
        const readyVersion = await getSystemVersion({ timeout: restartProbeTimeoutMs, silent: true })
        if (normalizeSystemVersion(readyVersion.version) === expectedVersion) {
          window.location.reload()
          return
        }
      }
      catch {
        // 进程切换期间短暂不可达，继续等待目标版本就绪。
      }

      const remainingMs = deadline - Date.now()
      if (remainingMs > 0) {
        await delay(Math.min(restartReadyPollIntervalMs, remainingMs))
      }
    }

    updateError.value = `服务未在预期时间内启动 v${expectedVersion}`
    setPhase({ kind: 'failed' })
  }

  async function restartNow() {
    if (restarting.value)
      return

    if (!restartTargetVersion.value) {
      const error = new Error('缺少待生效的目标版本')
      updateError.value = error.message
      setPhase({ kind: 'failed' })
      throw error
    }

    statusGeneration += 1
    statusPoll.pause()
    setPhase({ kind: 'restarting' })
    updateError.value = ''
    disconnectUpdateEvents()

    try {
      // 进程可能在返回响应前退出，由下面的目标版本探测判定是否完成。
      await restartSystem({ silent: true })
    }
    catch (error: unknown) {
      if (error instanceof ApiError && error.status > 0) {
        setPhase({ kind: 'failed' })
        updateError.value = errorMessage(error)
        throw error
      }
    }

    await waitForServiceAndReload()
  }

  return {
    version,
    updateInfo,
    phase,
    loading,
    checking,
    changingChannel,
    selectedChannel,
    availableChannels,
    canChangeChannel,
    restartTargetVersion,
    updating,
    restarting,
    updateError,
    lastFailedOperation,
    needRestart,
    loadedOnce,
    updateLogs,
    updateStreaming,
    updateStreamError,
    hasUpdate,
    hasCandidateUpdate,
    canUpdate,
    loadVersion,
    loadSystem,
    checkUpdates,
    changeChannel,
    updateNow,
    restartNow,
    connectUpdateEvents,
    disconnectUpdateEvents,
    clearUpdateLogs,
  }
})

export function normalizeSystemVersion(value: unknown) {
  return String(value ?? '')
    .trim()
    .replace(/^v/i, '')
}
