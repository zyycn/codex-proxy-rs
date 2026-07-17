import type { SystemUpdateEvent } from '@/api/streams/system-update'
import { defineStore } from 'pinia'

import { computed, ref, shallowRef } from 'vue'
import {
  getSystemUpdateDetail,
  getSystemVersion,
  performSystemUpdate,
  restartSystem,
} from '@/api'
import { ApiError } from '@/api/request'
import { openSystemUpdateStream } from '@/api/streams/system-update'
import { delay, reloadPage } from '@/platform/browser'
import { errorMessage } from '@/utils/async'
import { reduceSystemUpdateLogs } from './system-update-reducer'

const maxUpdateLogs = 200
const restartReadyTimeoutMs = 60_000
const restartProbeTimeoutMs = 2_000
const restartReadyPollIntervalMs = 500

type SystemUpdateStream = NonNullable<ReturnType<typeof openSystemUpdateStream>>

export const useSystemUpdateStore = defineStore('system-update', () => {
  const version = shallowRef<Awaited<ReturnType<typeof getSystemVersion>> | null>(null)
  const updateInfo = shallowRef<Awaited<ReturnType<typeof getSystemUpdateDetail>> | null>(null)
  const loading = shallowRef(false)
  const checking = shallowRef(false)
  const updating = shallowRef(false)
  const restarting = shallowRef(false)
  const updateError = shallowRef('')
  const updateSuccess = shallowRef(false)
  const needRestart = shallowRef(false)
  const loadedOnce = shallowRef(false)
  const updateLogs = ref<SystemUpdateEvent[]>([])
  const updateStreaming = shallowRef(false)
  const updateStreamError = shallowRef('')
  const restartTargetVersion = shallowRef('')

  let updateEventSource: SystemUpdateStream | null = null
  let loadVersionPromise: ReturnType<typeof getSystemVersion> | undefined
  let loadSystemPromise: Promise<void> | undefined

  const hasUpdate = computed(() => Boolean(updateInfo.value?.hasUpdate ?? version.value?.hasUpdate))
  const isReleaseBuild = computed(() => updateInfo.value?.buildType === 'release')
  const canUpdate = computed(
    () =>
      hasUpdate.value
      && isReleaseBuild.value
      && Boolean(updateInfo.value?.updateSupported)
      && !updating.value
      && !checking.value
      && !restarting.value,
  )

  function resetUpdateResult() {
    updateError.value = ''
    updateSuccess.value = false
    needRestart.value = false
    restartTargetVersion.value = ''
  }

  function appendUpdateLog(log: SystemUpdateEvent) {
    updateLogs.value = reduceSystemUpdateLogs(updateLogs.value, log, maxUpdateLogs)
  }

  function clearUpdateLogs() {
    updateLogs.value = []
    updateStreamError.value = ''
  }

  function connectUpdateEvents(force = false) {
    if (force) {
      disconnectUpdateEvents()
    }
    else if (updateEventSource) {
      return
    }

    updateStreamError.value = ''
    let source: SystemUpdateStream | null = null
    source = openSystemUpdateStream({
      onOpen: () => {
        if (updateEventSource !== source)
          return
        updateStreaming.value = true
        updateStreamError.value = ''
      },
      onEvent: (event) => {
        if (updateEventSource !== source)
          return
        appendUpdateLog(event)
        if (event.terminal)
          disconnectUpdateEvents(source)
      },
      onError: (closed) => {
        if (updateEventSource !== source)
          return
        updateStreaming.value = false
        updateStreamError.value = '更新日志连接中断'
        if (closed)
          updateEventSource = null
      },
      onParseError: () => {
        if (updateEventSource === source)
          updateStreamError.value = '更新日志解析失败'
      },
    })
    if (!source) {
      updateStreamError.value = '当前浏览器不支持实时更新日志'
      return
    }
    updateEventSource = source
  }

  function disconnectUpdateEvents(source = updateEventSource) {
    source?.close()
    if (source && updateEventSource !== source)
      return
    updateEventSource = null
    updateStreaming.value = false
  }

  async function loadSystem(refresh = false) {
    if (loadSystemPromise)
      return loadSystemPromise

    loading.value = true
    loadSystemPromise = (async () => {
      updateInfo.value = await getSystemUpdateDetail(refresh)
      if (!version.value)
        version.value = await getSystemVersion()
      loadedOnce.value = true
    })()

    try {
      await loadSystemPromise
    }
    finally {
      loading.value = false
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

    checking.value = true
    resetUpdateResult()
    try {
      updateInfo.value = await getSystemUpdateDetail(refresh)
      if (!version.value)
        version.value = await getSystemVersion()
      loadedOnce.value = true
      return updateInfo.value
    }
    finally {
      checking.value = false
    }
  }

  async function updateNow(targetVersion: string) {
    const currentInfo = updateInfo.value
    const confirmedTargetVersion = targetVersion.trim()
    if (!canUpdate.value || !currentInfo || updating.value || !confirmedTargetVersion)
      return null

    clearUpdateLogs()
    connectUpdateEvents(true)
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
    }
    catch (error: unknown) {
      updateError.value = errorMessage(error, '更新失败')
      appendUpdateLog({
        id: `update-client-error-${Date.now()}`,
        level: 'error',
        message: updateError.value,
        at: new Date().toISOString(),
      })
      throw error
    }
    finally {
      updating.value = false
    }
  }

  async function waitForServiceAndReload() {
    const expectedVersion = restartTargetVersion.value
    const deadline = Date.now() + restartReadyTimeoutMs

    while (Date.now() < deadline) {
      try {
        const readyVersion = await getSystemVersion({ timeoutMs: restartProbeTimeoutMs })
        if (normalizeVersion(readyVersion.version) === expectedVersion) {
          reloadPage()
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
    restarting.value = false
  }

  async function restartNow() {
    if (restarting.value)
      return

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
    }
    catch (error: unknown) {
      if (error instanceof ApiError && error.status > 0) {
        restarting.value = false
        updateError.value = errorMessage(error, '重启失败')
        throw error
      }
    }

    await waitForServiceAndReload()
  }

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
})

function normalizeVersion(value: unknown) {
  return String(value ?? '')
    .trim()
    .replace(/^v/i, '')
}
