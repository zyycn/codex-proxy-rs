import type { BackupSettingsView, UpdateBackupStoragePayload } from '@/api'

import { toast } from '@codex-proxy/ui'
import { computed, ref, shallowRef } from 'vue'
import {
  getBackupSettings,
  testBackupStorage,
  updateBackupSchedule,
  updateBackupStorage,
} from '@/api'

/** 存储与计划配置共用的加载/保存/测试 composable。 */
export function useBackupSettings() {
  const loading = shallowRef(false)
  const savingStorage = shallowRef(false)
  const testing = shallowRef(false)
  const savingSchedule = shallowRef(false)
  const busy = computed(() => loading.value || savingStorage.value || testing.value || savingSchedule.value)
  const loaded = shallowRef(false)

  let storageRevision = 0
  const verified = shallowRef(false)

  // 存储表单；secretAccessKey 由 GET 回填，保存时始终整体提交。
  const storage = ref({
    endpoint: '',
    region: 'auto',
    bucket: '',
    accessKeyId: '',
    secretAccessKey: '',
    prefix: 'backups/',
    forcePathStyle: false,
  })

  const schedule = ref({
    scheduleEnabled: false,
    cronExpression: '0 2 * * *',
    retentionDays: '7',
    retentionCount: '7',
  })

  async function load(): Promise<void> {
    if (loaded.value || busy.value)
      return
    loading.value = true
    try {
      const data = await getBackupSettings()
      applyStorage(data)
      applySchedule(data)
      loaded.value = true
    }
    catch {}
    finally {
      loading.value = false
    }
  }

  function applyStorage(data: BackupSettingsView): void {
    storageRevision = data.storageRevision
    verified.value = data.verified
    storage.value = {
      endpoint: data.endpoint ?? '',
      region: data.region ?? 'auto',
      bucket: data.bucket ?? '',
      accessKeyId: data.accessKeyId ?? '',
      secretAccessKey: data.secretAccessKey ?? '',
      prefix: data.prefix ?? 'backups/',
      forcePathStyle: data.forcePathStyle,
    }
  }

  function applySchedule(data: BackupSettingsView): void {
    schedule.value = {
      scheduleEnabled: data.scheduleEnabled,
      cronExpression: data.cronExpression ?? '0 2 * * *',
      retentionDays: String(data.retentionDays),
      retentionCount: String(data.retentionCount),
    }
  }

  function storagePayload(): UpdateBackupStoragePayload {
    return {
      endpoint: storage.value.endpoint.trim(),
      region: storage.value.region.trim(),
      bucket: storage.value.bucket.trim(),
      accessKeyId: storage.value.accessKeyId.trim(),
      secretAccessKey: storage.value.secretAccessKey.trim(),
      prefix: storage.value.prefix.trim(),
      forcePathStyle: storage.value.forcePathStyle,
    }
  }

  async function saveStorage(): Promise<boolean> {
    if (busy.value || !loaded.value)
      return false
    savingStorage.value = true
    try {
      const data = await updateBackupStorage(storagePayload())
      const storageChanged = data.storageRevision !== storageRevision
      const schedulePaused = storageChanged && schedule.value.scheduleEnabled && !data.scheduleEnabled
      applyStorage(data)
      // 仅存储版本变化时同步服务端暂停状态，普通保存不覆盖计划草稿。
      if (storageChanged)
        schedule.value.scheduleEnabled = data.scheduleEnabled
      toast.success(schedulePaused ? '存储配置已保存，定时备份已暂停，请测试连接后重新启用' : '存储配置已保存')
      return true
    }
    catch {
      return false
    }
    finally {
      savingStorage.value = false
    }
  }

  async function runTest(): Promise<void> {
    if (busy.value || !loaded.value)
      return
    testing.value = true
    try {
      const result = await testBackupStorage()
      if (result.ok) {
        verified.value = true
        toast.success('连接测试通过')
      }
      else {
        verified.value = false
        toast.error(`${result.stage}: ${result.message}`)
      }
    }
    catch {
      verified.value = false
    }
    finally {
      testing.value = false
    }
  }

  async function saveSchedule(): Promise<boolean> {
    if (busy.value || !loaded.value)
      return false
    savingSchedule.value = true
    try {
      const data = await updateBackupSchedule({
        scheduleEnabled: schedule.value.scheduleEnabled,
        cronExpression: schedule.value.cronExpression.trim(),
        retentionDays: Number(schedule.value.retentionDays) || 0,
        retentionCount: Number(schedule.value.retentionCount) || 0,
      })
      applySchedule(data)
      toast.success('备份计划已保存')
      return true
    }
    catch {
      return false
    }
    finally {
      savingSchedule.value = false
    }
  }

  return {
    loading,
    busy,
    savingStorage,
    testing,
    savingSchedule,
    loaded,
    verified,
    storage,
    schedule,
    load,
    saveStorage,
    runTest,
    saveSchedule,
  }
}
