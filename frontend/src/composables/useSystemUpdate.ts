import { computed, ref, shallowRef } from 'vue'

import {
  SYSTEM_UPDATE_EVENTS_URL,
  getSystemVersion,
  getSystemUpdateDetail,
  performSystemUpdate,
  restartSystem,
  type SystemUpdateInfo,
  type SystemVersion,
} from '@/api'
import { ApiError } from '@/api/request'

export type SystemUpdateLogLevel = 'info' | 'success' | 'warning' | 'error'

export interface SystemUpdateLog {
  id: string
  operationId?: string
  level: SystemUpdateLogLevel
  step?: string
  message: string
  terminal?: boolean
  at: string
}

const version = shallowRef<SystemVersion | null>(null)
const updateInfo = shallowRef<SystemUpdateInfo | null>(null)
const loading = shallowRef(false)
const checking = shallowRef(false)
const updating = shallowRef(false)
const restarting = shallowRef(false)
const updateError = shallowRef('')
const updateSuccess = shallowRef(false)
const needRestart = shallowRef(false)
const loadedOnce = shallowRef(false)
const updateLogs = ref<SystemUpdateLog[]>([])
const updateStreaming = shallowRef(false)
const updateStreamError = shallowRef('')
const updateEventSource = shallowRef<EventSource | null>(null)
const restartTargetVersion = shallowRef('')

let loadVersionPromise: Promise<SystemVersion> | undefined
let loadSystemPromise: Promise<void> | undefined
const maxUpdateLogs = 200
const restartReadyTimeoutMs = 60_000
const restartProbeTimeoutMs = 2_000
const restartReadyPollIntervalMs = 500

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
  restartTargetVersion.value = ''
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

function connectUpdateEvents(options: { force?: boolean } = {}) {
  if (options.force) {
    disconnectUpdateEvents()
  } else if (updateEventSource.value) {
    return
  }

  if (typeof EventSource === 'undefined') {
    updateStreamError.value = '当前浏览器不支持实时更新日志'
    return
  }

  updateStreamError.value = ''
  const source = new EventSource(SYSTEM_UPDATE_EVENTS_URL, { withCredentials: true })
  updateEventSource.value = source

  source.onopen = () => {
    if (updateEventSource.value !== source) return

    updateStreaming.value = true
    updateStreamError.value = ''
  }

  source.addEventListener('update', (event) => {
    try {
      const log = JSON.parse((event as MessageEvent<string>).data)
      if (isUpdateLog(log)) {
        appendUpdateLog(log)
        if (log.terminal) {
          disconnectUpdateEvents(source)
        }
      }
    } catch {
      updateStreamError.value = '更新日志解析失败'
    }
  })

  source.onerror = () => {
    if (updateEventSource.value !== source) return

    updateStreaming.value = false
    updateStreamError.value = '更新日志连接中断'
    if (source.readyState === EventSource.CLOSED) {
      updateEventSource.value = null
    }
  }
}

function disconnectUpdateEvents(source: EventSource | null = updateEventSource.value) {
  source?.close()
  if (source && updateEventSource.value !== source) return

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
  connectUpdateEvents({ force: true })
  updating.value = true
  updateError.value = ''
  updateSuccess.value = false
  try {
    const result = await performSystemUpdate(confirmedTargetVersion)
    updateSuccess.value = true
    needRestart.value = result.needRestart
    restartTargetVersion.value = result.needRestart ? normalizeVersion(result.targetVersion) : ''
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

function delay(ms: number) {
  return new Promise<void>((resolve) => {
    window.setTimeout(resolve, ms)
  })
}

async function waitForServiceAndReload() {
  const expectedVersion = restartTargetVersion.value
  const deadline = Date.now() + restartReadyTimeoutMs

  while (Date.now() < deadline) {
    try {
      const readyVersion = await getSystemVersion({ timeoutMs: restartProbeTimeoutMs })
      if (normalizeVersion(readyVersion.version) === expectedVersion) {
        window.location.reload()
        return
      }
    } catch {
      // 进程切换期间短暂不可达，继续等待目标版本就绪
    }

    const remainingMs = deadline - Date.now()
    if (remainingMs > 0) {
      await delay(Math.min(restartReadyPollIntervalMs, remainingMs))
    }
  }

  updateError.value = `服务未在预期时间内启动 v${expectedVersion}`
  restarting.value = false
}

function normalizeVersion(value: unknown) {
  return String(value ?? '')
    .trim()
    .replace(/^v/i, '')
}

function apiErrorStatus(error: unknown) {
  return error instanceof ApiError ? error.status : 0
}

async function restartNow() {
  if (restarting.value) return

  if (!restartTargetVersion.value) {
    const error = new Error('缺少待生效的目标版本')
    updateError.value = error.message
    throw error
  }

  restarting.value = true
  updateError.value = ''
  disconnectUpdateEvents()

  try {
    await restartSystem()
  } catch (error) {
    if (apiErrorStatus(error) > 0) {
      restarting.value = false
      updateError.value = error instanceof Error ? error.message : '重启失败'
      throw error
    }
  }

  await waitForServiceAndReload()
}

export function useSystemUpdate() {
  return {
    version,
    updateInfo,
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
}
