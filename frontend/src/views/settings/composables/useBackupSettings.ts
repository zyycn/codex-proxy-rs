import type { BackupSettingsView, UpdateBackupStoragePayload } from '@/api'

import { reactive, shallowRef } from 'vue'
import {
  getBackupSettings,
  testBackupStorage,
  updateBackupSchedule,
  updateBackupStorage,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'

/** 存储与计划配置共用的加载/保存/测试 composable。 */
export function useBackupSettings() {
  const loading = shallowRef(false)
  const savingStorage = shallowRef(false)
  const testing = shallowRef(false)
  const savingSchedule = shallowRef(false)
  const error = shallowRef('')
  const loaded = shallowRef(false)

  const storageRevision = shallowRef(0)
  const verified = shallowRef(false)

  // 存储表单；secretAccessKey 由 GET 回填，保存时始终整体提交。
  const storage = reactive({
    endpoint: '',
    region: 'auto',
    bucket: '',
    accessKeyId: '',
    secretAccessKey: '',
    prefix: 'backups/',
    forcePathStyle: false,
  })

  const schedule = reactive({
    scheduleEnabled: false,
    cronExpression: '0 2 * * *',
    scheduleTimezone: 'Asia/Shanghai',
    retentionDays: '7',
    retentionCount: '7',
  })

  async function load(): Promise<void> {
    if (loaded.value)
      return
    loading.value = true
    error.value = ''
    try {
      const data = await getBackupSettings()
      applySettings(data)
      loaded.value = true
    }
    catch (cause) {
      error.value = errorMessage(cause, '加载备份配置失败')
    }
    finally {
      loading.value = false
    }
  }

  function applySettings(data: BackupSettingsView): void {
    storageRevision.value = data.storageRevision
    verified.value = data.verified
    storage.endpoint = data.endpoint ?? ''
    storage.region = data.region ?? 'auto'
    storage.bucket = data.bucket ?? ''
    storage.accessKeyId = data.accessKeyId ?? ''
    storage.secretAccessKey = data.secretAccessKey ?? ''
    storage.prefix = data.prefix ?? 'backups/'
    storage.forcePathStyle = data.forcePathStyle
    schedule.scheduleEnabled = data.scheduleEnabled
    schedule.cronExpression = data.cronExpression ?? '0 2 * * *'
    schedule.scheduleTimezone = data.scheduleTimezone ?? 'Asia/Shanghai'
    schedule.retentionDays = String(data.retentionDays)
    schedule.retentionCount = String(data.retentionCount)
  }

  function storagePayload(): UpdateBackupStoragePayload {
    return {
      endpoint: storage.endpoint.trim(),
      region: storage.region.trim(),
      bucket: storage.bucket.trim(),
      accessKeyId: storage.accessKeyId.trim(),
      secretAccessKey: storage.secretAccessKey.trim(),
      prefix: storage.prefix.trim(),
      forcePathStyle: storage.forcePathStyle,
    }
  }

  async function saveStorage(): Promise<boolean> {
    savingStorage.value = true
    try {
      const data = await updateBackupStorage(storagePayload())
      applySettings(data)
      verified.value = false
      toast.success('存储配置已保存')
      return true
    }
    catch (cause) {
      toast.error(errorMessage(cause, '保存存储配置失败'))
      return false
    }
    finally {
      savingStorage.value = false
    }
  }

  async function runTest(): Promise<void> {
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
    catch (cause) {
      verified.value = false
      toast.error(errorMessage(cause, '连接测试失败'))
    }
    finally {
      testing.value = false
    }
  }

  async function saveSchedule(): Promise<boolean> {
    savingSchedule.value = true
    try {
      const data = await updateBackupSchedule({
        scheduleEnabled: schedule.scheduleEnabled,
        cronExpression: schedule.cronExpression.trim(),
        scheduleTimezone: schedule.scheduleTimezone.trim(),
        retentionDays: Number(schedule.retentionDays) || 0,
        retentionCount: Number(schedule.retentionCount) || 0,
      })
      applySettings(data)
      toast.success('调度配置已保存')
      return true
    }
    catch (cause) {
      toast.error(errorMessage(cause, '保存调度配置失败'))
      return false
    }
    finally {
      savingSchedule.value = false
    }
  }

  return {
    loading,
    savingStorage,
    testing,
    savingSchedule,
    error,
    loaded,
    storageRevision,
    verified,
    storage,
    schedule,
    load,
    saveStorage,
    runTest,
    saveSchedule,
  }
}

export type BackupSettingsStore = ReturnType<typeof useBackupSettings>
