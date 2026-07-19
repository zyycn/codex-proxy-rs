import type { rotationOptions } from '../constants'
import { computed, onMounted, reactive, ref, shallowRef } from 'vue'

import { getSettings, updateSettings } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'

type RotationStrategy = (typeof rotationOptions)[number]['value']

export function useSettingsForm() {
  const loading = shallowRef(true)
  const saving = shallowRef(false)
  const error = shallowRef('')
  const mappings = ref<Record<string, Array<{ requestedModel: string, upstreamModel: string }>>>({
    openai: [],
    xai: [],
  })
  const form = reactive({
    configRevision: 0,
    refreshMarginSeconds: null as number | null,
    refreshConcurrency: null as number | null,
    maxConcurrentPerAccount: null as number | null,
    requestIntervalMs: null as number | null,
    rotationStrategy: '' as RotationStrategy | '',
    usageRetentionDays: 31,
    opsEventRetentionDays: 30,
    auditRetentionDays: 90,
  })

  function numericModel(key: 'refreshMarginSeconds' | 'refreshConcurrency' | 'maxConcurrentPerAccount' | 'requestIntervalMs') {
    return computed({
      get: () => (form[key] === null ? '' : String(form[key])),
      set: (value: string) => {
        if (!value.trim()) {
          form[key] = null
          return
        }
        const parsed = Number(value)
        form[key] = Number.isFinite(parsed) ? parsed : null
      },
    })
  }

  const refreshMarginSecondsValue = numericModel('refreshMarginSeconds')
  const refreshConcurrencyValue = numericModel('refreshConcurrency')
  const maxConcurrentPerAccountValue = numericModel('maxConcurrentPerAccount')
  const requestIntervalMsValue = numericModel('requestIntervalMs')

  function applySettings(data: Awaited<ReturnType<typeof getSettings>>) {
    form.configRevision = data.configRevision
    form.refreshMarginSeconds = data.refreshMarginSeconds
    form.refreshConcurrency = data.refreshConcurrency
    form.maxConcurrentPerAccount = data.maxConcurrentPerAccount
    form.requestIntervalMs = data.requestIntervalMs
    form.rotationStrategy = data.rotationStrategy
    form.usageRetentionDays = data.usageRetentionDays
    form.opsEventRetentionDays = data.opsEventRetentionDays
    form.auditRetentionDays = data.auditRetentionDays
    mappings.value = Object.fromEntries(
      ['openai', 'xai'].map(provider => [
        provider,
        Object.entries(data.providerModelMappings?.[provider] || {}).map(([requestedModel, upstreamModel]) => ({
          requestedModel,
          upstreamModel: String(upstreamModel),
        })),
      ]),
    )
  }

  async function loadSettings() {
    loading.value = true
    error.value = ''
    try {
      applySettings(await getSettings())
    }
    catch (cause: unknown) {
      error.value = errorMessage(cause, '设置加载失败')
      toast.error(error.value)
    }
    finally {
      loading.value = false
    }
  }

  function addMapping(provider: string) {
    mappings.value = {
      ...mappings.value,
      [provider]: [...(mappings.value[provider] || []), { requestedModel: '', upstreamModel: '' }],
    }
  }

  function updateMapping(provider: string, index: number, key: string, value: string) {
    const rows = [...(mappings.value[provider] || [])]
    if (!rows[index])
      return
    rows[index] = { ...rows[index], [key]: value }
    mappings.value = { ...mappings.value, [provider]: rows }
  }

  function removeMapping(provider: string, index: number) {
    const rows = [...(mappings.value[provider] || [])]
    rows.splice(index, 1)
    mappings.value = { ...mappings.value, [provider]: rows }
  }

  function mappingPayload() {
    const payload: Record<string, Record<string, string>> = {}
    for (const provider of ['openai', 'xai']) {
      const entries: Record<string, string> = {}
      for (const row of mappings.value[provider] || []) {
        const requested = row.requestedModel.trim()
        const upstream = row.upstreamModel.trim()
        if (!requested || !upstream)
          throw new Error('请完整填写模型映射')
        if (entries[requested])
          throw new Error(`${provider} 存在重复的客户端模型：${requested}`)
        entries[requested] = upstream
      }
      payload[provider] = entries
    }
    return payload
  }

  async function saveSettings() {
    if (saving.value || loading.value)
      return
    const { refreshMarginSeconds, refreshConcurrency, maxConcurrentPerAccount, requestIntervalMs, rotationStrategy } = form
    if (refreshMarginSeconds === null || refreshConcurrency === null || maxConcurrentPerAccount === null || requestIntervalMs === null || !rotationStrategy) {
      toast.warning('请完整填写运行参数和调度策略')
      return
    }
    try {
      saving.value = true
      const result = await updateSettings({
        expectedConfigRevision: form.configRevision,
        providerModelMappings: mappingPayload(),
        refreshMarginSeconds,
        refreshConcurrency,
        maxConcurrentPerAccount,
        requestIntervalMs,
        rotationStrategy,
        usageRetentionDays: form.usageRetentionDays,
        opsEventRetentionDays: form.opsEventRetentionDays,
        auditRetentionDays: form.auditRetentionDays,
      })
      applySettings(result)
      toast.success('设置已保存')
    }
    catch (cause: unknown) {
      error.value = errorMessage(cause, '保存失败')
      toast.error(error.value)
      await loadSettings()
    }
    finally {
      saving.value = false
    }
  }

  onMounted(() => {
    void loadSettings()
  })

  return {
    loading,
    saving,
    error,
    form,
    mappings,
    addMapping,
    updateMapping,
    removeMapping,
    refreshMarginSecondsValue,
    refreshConcurrencyValue,
    maxConcurrentPerAccountValue,
    requestIntervalMsValue,
    saveSettings,
  }
}
