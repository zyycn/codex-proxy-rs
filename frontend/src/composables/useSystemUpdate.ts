import { computed, ref, shallowRef } from 'vue'

import {
  SYSTEM_UPDATE_EVENTS_URL,
  checkSystemHealth,
  getSystemVersion,
  getSystemUpdateDetail,
  performSystemUpdate,
  restartSystem,
  type SystemUpdateInfo,
  type SystemVersion,
} from '@/api'

export type SystemUpdateLogLevel = 'info' | 'success' | 'warning' | 'error'

export interface SystemUpdateLog {
  id: string
  operationId?: string
  level: SystemUpdateLogLevel
  step?: string
  message: string
  at: string
}

const version = shallowRef<SystemVersion | null>(null)
const updateInfo = shallowRef<SystemUpdateInfo | null>(null)
const loading = shallowRef(false)
const checking = shallowRef(false)
const updating = shallowRef(false)
const restarting = shallowRef(false)
const restartCountdown = shallowRef(0)
const updateError = shallowRef('')
const updateSuccess = shallowRef(false)
const needRestart = shallowRef(false)
const loadedOnce = shallowRef(false)
const updateLogs = ref<SystemUpdateLog[]>([])
const updateStreaming = shallowRef(false)
const updateStreamError = shallowRef('')
const updateEventSource = shallowRef<EventSource | null>(null)

let restartTimer: number | undefined
let loadVersionPromise: Promise<SystemVersion> | undefined
let loadSystemPromise: Promise<void> | undefined
const maxUpdateLogs = 200
const restartCountdownSeconds = 8
const restartReadyPollAttempts = 60
const restartReadyPollIntervalMs = 1000

const hasUpdate = computed(() => Boolean(updateInfo.value?.hasUpdate ?? version.value?.hasUpdate))
const isReleaseBuild = computed(() => updateInfo.value?.buildType === 'release')
const canUpdate = computed(
  () =>
    hasUpdate.value &&
    isReleaseBuild.value &&
    Boolean(updateInfo.value?.updateSupported) &&
    !updating.value &&
    !checking.value &&
    !restarting.value,
)

function resetUpdateResult() {
  updateError.value = ''
  updateSuccess.value = false
  needRestart.value = false
}

function isUpdateLog(value: unknown): value is SystemUpdateLog {
  return (
    typeof value === 'object' &&
    value !== null &&
    'id' in value &&
    'level' in value &&
    'message' in value &&
    'at' in value
  )
}

function appendUpdateLog(log: SystemUpdateLog) {
  updateLogs.value = [...updateLogs.value, log].slice(-maxUpdateLogs)
}

function clearUpdateLogs() {
  updateLogs.value = []
  updateStreamError.value = ''
}

function connectUpdateEvents() {
  if (updateEventSource.value) return

  if (typeof EventSource === 'undefined') {
    updateStreamError.value = '当前浏览器不支持实时更新日志'
    return
  }

  updateStreamError.value = ''
  const source = new EventSource(SYSTEM_UPDATE_EVENTS_URL, { withCredentials: true })
  updateEventSource.value = source

  source.onopen = () => {
    updateStreaming.value = true
    updateStreamError.value = ''
  }

  source.addEventListener('update', (event) => {
    try {
      const log = JSON.parse((event as MessageEvent<string>).data)
      if (isUpdateLog(log)) {
        appendUpdateLog(log)
      }
    } catch {
      updateStreamError.value = '更新日志解析失败'
    }
  })

  source.onerror = () => {
    updateStreaming.value = false
    updateStreamError.value = '更新日志连接中断'
    if (source.readyState === EventSource.CLOSED) {
      updateEventSource.value = null
    }
  }
}

function disconnectUpdateEvents() {
  updateEventSource.value?.close()
  updateEventSource.value = null
  updateStreaming.value = false
}

async function loadSystem(refresh = false) {
  if (loadSystemPromise) return loadSystemPromise

  loading.value = true
  loadSystemPromise = (async () => {
    const updateData = await getSystemUpdateDetail(refresh)
    updateInfo.value = updateData
    if (!version.value) {
      version.value = await getSystemVersion()
    }
    loadedOnce.value = true
  })()

  try {
    await loadSystemPromise
  } finally {
    loading.value = false
    loadSystemPromise = undefined
  }
}

async function loadVersion() {
  if (loadVersionPromise) return loadVersionPromise

  loadVersionPromise = getSystemVersion()
  try {
    const versionData = await loadVersionPromise
    version.value = versionData
    return versionData
  } finally {
    loadVersionPromise = undefined
  }
}

async function checkUpdates(refresh = true) {
  if (checking.value) return updateInfo.value

  checking.value = true
  resetUpdateResult()
  try {
    updateInfo.value = await getSystemUpdateDetail(refresh)
    if (!version.value) {
      version.value = await getSystemVersion()
    }
    loadedOnce.value = true
    return updateInfo.value
  } finally {
    checking.value = false
  }
}

async function updateNow(targetVersion: string) {
  const currentInfo = updateInfo.value
  const confirmedTargetVersion = targetVersion.trim()
  if (!canUpdate.value || !currentInfo || updating.value || !confirmedTargetVersion) return null

  clearUpdateLogs()
  connectUpdateEvents()
  updating.value = true
  updateError.value = ''
  updateSuccess.value = false
  try {
    const result = await performSystemUpdate(confirmedTargetVersion)
    updateSuccess.value = true
    needRestart.value = result.needRestart
    updateInfo.value = {
      ...currentInfo,
      latestVersion: result.targetVersion,
      hasUpdate: false,
    }
    return result
  } catch (error: any) {
    updateError.value = error.message || '更新失败'
    appendUpdateLog({
      id: `update-client-error-${Date.now()}`,
      level: 'error',
      message: updateError.value,
      at: new Date().toISOString(),
    })
    throw error
  } finally {
    updating.value = false
  }
}

function clearRestartTimer() {
  if (restartTimer !== undefined) {
    window.clearInterval(restartTimer)
    restartTimer = undefined
  }
}

function delay(ms: number) {
  return new Promise<void>((resolve) => {
    window.setTimeout(resolve, ms)
  })
}

async function waitForServiceAndReload() {
  restartCountdown.value = 0

  for (let attempt = 0; attempt < restartReadyPollAttempts; attempt += 1) {
    try {
      await checkSystemHealth()
      window.location.reload()
      return
    } catch {
      if (attempt < restartReadyPollAttempts - 1) {
        await delay(restartReadyPollIntervalMs)
      }
    }
  }

  updateError.value = '服务重启恢复超时，请检查进程是否已被自重启或外部守护进程拉起'
  restarting.value = false
  restartCountdown.value = 0
}

function apiErrorStatus(error: unknown) {
  if (typeof error !== 'object' || error === null || !('status' in error)) {
    return 0
  }

  const status = (error as { status?: unknown }).status
  return typeof status === 'number' ? status : 0
}

async function restartNow() {
  if (restarting.value) return

  restarting.value = true
  restartCountdown.value = restartCountdownSeconds
  updateError.value = ''
  clearRestartTimer()
  disconnectUpdateEvents()

  try {
    await restartSystem()
  } catch (error) {
    // The connection can drop immediately after the restart request is accepted.
    if (apiErrorStatus(error) > 0) {
      restarting.value = false
      restartCountdown.value = 0
      updateError.value = error instanceof Error ? error.message : '重启失败'
      throw error
    }
  }

  restartTimer = window.setInterval(() => {
    restartCountdown.value -= 1
    if (restartCountdown.value <= 0) {
      clearRestartTimer()
      void waitForServiceAndReload()
    }
  }, 1000)
}

export function useSystemUpdate() {
  return {
    version,
    updateInfo,
    loading,
    checking,
    updating,
    restarting,
    restartCountdown,
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
    clearRestartTimer,
    connectUpdateEvents,
    disconnectUpdateEvents,
    clearUpdateLogs,
  }
}
