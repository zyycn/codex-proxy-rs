import { until, useEventSource } from '@vueuse/core'
import { delay } from 'es-toolkit'
import { defineStore } from 'pinia'

import { computed, ref, shallowRef, watch } from 'vue'
import {
  getSystemUpdateDetail,
  getSystemVersion,
  performSystemUpdate,
  restartSystem,
} from '@/api'
import { API_BASE_URL } from '@/api/constants'
import { ApiError } from '@/api/request'
import { errorMessage } from '@/utils/async'

const maxUpdateLogs = 200
const updateEventReadyTimeoutMs = 3_000
const restartReadyTimeoutMs = 60_000
const restartProbeTimeoutMs = 2_000
const restartReadyPollIntervalMs = 500

interface SystemUpdateEvent {
  id: string
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
    | { kind: 'ready' }
    | { kind: 'updating' }
    | { kind: 'restart_required' }
    | { kind: 'restarting' }
    | { kind: 'failed' }

const UPDATE_BUSY_PHASES = new Set<SystemUpdatePhase['kind']>([
  'loading',
  'checking',
  'updating',
  'restarting',
])

export const useSystemUpdateStore = defineStore('system-update', () => {
  const version = shallowRef<Awaited<ReturnType<typeof getSystemVersion>> | null>(null)
  const updateInfo = shallowRef<Awaited<ReturnType<typeof getSystemUpdateDetail>> | null>(null)
  const phase = shallowRef<SystemUpdatePhase>({ kind: 'idle' })
  const updateError = shallowRef('')
  const updateSuccess = shallowRef(false)
  const needRestart = shallowRef(false)
  const loadedOnce = shallowRef(false)
  const updateLogs = ref<SystemUpdateEvent[]>([])
  const updateStreaming = shallowRef(false)
  const updateStreamError = shallowRef('')
  const restartTargetVersion = shallowRef('')
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

  const phaseKind = computed(() => phase.value.kind)
  const loading = computed(() => phaseKind.value === 'loading')
  const checking = computed(() => phaseKind.value === 'checking')
  const updating = computed(() => phaseKind.value === 'updating')
  const restarting = computed(() => phaseKind.value === 'restarting')

  const hasUpdate = computed(() => Boolean(updateInfo.value?.hasUpdate ?? version.value?.hasUpdate))
  const isReleaseBuild = computed(() => updateInfo.value?.buildType === 'release')
  const canUpdate = computed(
    () =>
      hasUpdate.value
      && isReleaseBuild.value
      && Boolean(updateInfo.value?.updateSupported)
      && !UPDATE_BUSY_PHASES.has(phaseKind.value),
  )

  function setPhase(next: SystemUpdatePhase) {
    phase.value = next
  }

  function resetUpdateResult() {
    updateError.value = ''
    updateSuccess.value = false
    needRestart.value = false
    restartTargetVersion.value = ''
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
      appendUpdateLog(event)
      if (event.terminal)
        disconnectUpdateEvents()
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

  async function loadSystem(refresh = false) {
    if (loadSystemPromise)
      return loadSystemPromise

    if (!UPDATE_BUSY_PHASES.has(phaseKind.value)) {
      setPhase({ kind: 'loading' })
    }
    loadSystemPromise = (async () => {
      updateInfo.value = await getSystemUpdateDetail({ refresh })
      if (!version.value)
        version.value = await getSystemVersion()
      loadedOnce.value = true
    })()

    try {
      await loadSystemPromise
    }
    finally {
      if (phaseKind.value === 'loading')
        setPhase({ kind: 'ready' })
      loadSystemPromise = undefined
    }
  }

  async function loadVersion() {
    if (loadVersionPromise)
      return loadVersionPromise

    loadVersionPromise = getSystemVersion()
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
    if (checking.value)
      return updateInfo.value

    setPhase({ kind: 'checking' })
    resetUpdateResult()
    try {
      updateInfo.value = await getSystemUpdateDetail({ refresh })
      if (!version.value)
        version.value = await getSystemVersion()
      loadedOnce.value = true
      return updateInfo.value
    }
    finally {
      if (phaseKind.value === 'checking')
        setPhase({ kind: 'ready' })
    }
  }

  async function updateNow(targetVersion: string) {
    const currentInfo = updateInfo.value
    const confirmedTargetVersion = targetVersion.trim()
    if (!canUpdate.value || !currentInfo || updating.value || !confirmedTargetVersion)
      return null

    clearUpdateLogs()
    await connectUpdateEvents(true)
    setPhase({ kind: 'updating' })
    updateError.value = ''
    updateSuccess.value = false
    try {
      const result = await performSystemUpdate({ targetVersion: confirmedTargetVersion })
      updateSuccess.value = true
      needRestart.value = result.needRestart
      restartTargetVersion.value = result.needRestart ? normalizeVersion(result.targetVersion) : ''
      updateInfo.value = {
        ...currentInfo,
        latestVersion: result.targetVersion,
        hasUpdate: false,
      }
      setPhase(result.needRestart ? { kind: 'restart_required' } : { kind: 'ready' })
      return result
    }
    catch (error: unknown) {
      updateError.value = errorMessage(error, '更新失败')
      appendUpdateLog({
        id: `update-client-error-${Date.now()}`,
        level: 'error',
        message: updateError.value,
        at: new Date().toISOString(),
      })
      setPhase({ kind: 'failed' })
      throw error
    }
  }

  async function waitForServiceAndReload() {
    const expectedVersion = restartTargetVersion.value
    const deadline = Date.now() + restartReadyTimeoutMs

    while (Date.now() < deadline) {
      try {
        const readyVersion = await getSystemVersion(restartProbeTimeoutMs)
        if (normalizeVersion(readyVersion.version) === expectedVersion) {
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

    setPhase({ kind: 'restarting' })
    updateError.value = ''
    disconnectUpdateEvents()

    try {
      await restartSystem()
    }
    catch (error: unknown) {
      if (error instanceof ApiError && error.status > 0) {
        setPhase({ kind: 'failed' })
        updateError.value = errorMessage(error, '重启失败')
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
    updating,
    restarting,
    updateError,
    updateSuccess,
    needRestart,
    loadedOnce,
    updateLogs,
    updateStreaming,
    updateStreamError,
    hasUpdate,
    isReleaseBuild,
    canUpdate,
    loadVersion,
    loadSystem,
    checkUpdates,
    updateNow,
    restartNow,
    connectUpdateEvents,
    disconnectUpdateEvents,
    clearUpdateLogs,
  }
})

function normalizeVersion(value: unknown) {
  return String(value ?? '')
    .trim()
    .replace(/^v/i, '')
}
