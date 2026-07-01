import { computed, ref, shallowRef } from 'vue'

import {
  SYSTEM_UPDATE_EVENTS_URL,
  checkSystemUpdates,
  getSystemVersion,
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
const maxUpdateLogs = 200

const hasUpdate = computed(() => Boolean(updateInfo.value?.hasUpdate))
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

async function loadSystem(force = false) {
  if (loading.value) return

  loading.value = true
  try {
    const [versionData, updateData] = await Promise.all([
      getSystemVersion(),
      checkSystemUpdates(force),
    ])
    version.value = versionData
    updateInfo.value = updateData
    loadedOnce.value = true
  } finally {
    loading.value = false
  }
}

async function checkUpdates(force = true) {
  if (checking.value) return updateInfo.value

  checking.value = true
  resetUpdateResult()
  try {
    updateInfo.value = await checkSystemUpdates(force)
    if (!version.value) {
      version.value = await getSystemVersion()
    }
    loadedOnce.value = true
    return updateInfo.value
  } finally {
    checking.value = false
  }
}

async function updateNow() {
  if (!canUpdate.value || !updateInfo.value || updating.value) return null

  clearUpdateLogs()
  connectUpdateEvents()
  updating.value = true
  updateError.value = ''
  updateSuccess.value = false
  try {
    const result = await performSystemUpdate()
    updateSuccess.value = true
    needRestart.value = result.needRestart
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

async function waitForServiceAndReload() {
  for (let attempt = 0; attempt < 5; attempt += 1) {
    try {
      await getSystemVersion()
      window.location.reload()
      return
    } catch {
      await new Promise((resolve) => window.setTimeout(resolve, 1000))
    }
  }

  window.location.reload()
}

async function restartNow() {
  if (restarting.value) return

  restarting.value = true
  restartCountdown.value = 8
  clearRestartTimer()

  try {
    await restartSystem()
  } catch {
    // The connection can drop immediately after the restart request is accepted.
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
