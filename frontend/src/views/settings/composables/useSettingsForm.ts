import type { rotationOptions } from '../constants'
import type { RequestLocation } from '@/api'
import { computed, reactive, ref, shallowRef } from 'vue'

import { getSettings, updateSettings } from '@/api'
import { ApiError } from '@/api/request'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { errorMessage } from '@/utils/async'
import { normalizeRequestLocation, requestLocationError } from '@/utils/request-location'

type RotationStrategy = (typeof rotationOptions)[number]['value']

const MIB = 1024 * 1024

export function useSettingsForm() {
  const loading = shallowRef(true)
  const saveAction = useAsyncAction()
  const saving = saveAction.loading
  const error = shallowRef('')
  const mappings = ref<Array<{ requestedModel: string, upstreamModel: string }>>([])
  const savedRequestLocation = shallowRef<RequestLocation>()
  const form = reactive({
    requestLocationEnabled: false,
    requestLocation: { country: '', region: '', city: '', timezone: '' },
    refreshMarginSeconds: null as number | null,
    refreshConcurrency: null as number | null,
    maxConcurrentPerAccount: null as number | null,
    requestIntervalMs: null as number | null,
    maxWaitingPerKey: null as number | null,
    maxWaitingPerAccount: null as number | null,
    concurrencyWaitTimeoutSeconds: null as number | null,
    responsesMaxDecompressedBodyMiB: null as number | null,

    rotationStrategy: '' as RotationStrategy | '',
    minCodexDesktopVersion: '',
    minCodexCliVersion: '',
    usageRetentionDays: 31,
    opsEventRetentionDays: 30,
    auditRetentionDays: 90,

    accountAutoFreezeEnabled: false,
    accountAutoFreezeThreshold: null as number | null,
    accountAutoFreezeWindowSeconds: null as number | null,
    accountAutoFreezeDurationSeconds: null as number | null,
    accountAutoFreezeProbeEnabled: true,
    accountAutoFreezeProbeModel: '',
    accountAutoFreezeAdaptiveConcurrency: true,
  })

  function numericModel(key: 'refreshMarginSeconds' | 'refreshConcurrency' | 'maxConcurrentPerAccount' | 'requestIntervalMs' | 'maxWaitingPerKey' | 'maxWaitingPerAccount' | 'concurrencyWaitTimeoutSeconds' | 'responsesMaxDecompressedBodyMiB' | 'accountAutoFreezeThreshold' | 'accountAutoFreezeWindowSeconds' | 'accountAutoFreezeDurationSeconds') {
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
  const maxWaitingPerKeyValue = numericModel('maxWaitingPerKey')
  const maxWaitingPerAccountValue = numericModel('maxWaitingPerAccount')
  const responsesMaxDecompressedBodyMiBValue = numericModel('responsesMaxDecompressedBodyMiB')
  const concurrencyWaitTimeoutSecondsValue = numericModel('concurrencyWaitTimeoutSeconds')
  const accountAutoFreezeThresholdValue = numericModel('accountAutoFreezeThreshold')
  const accountAutoFreezeWindowSecondsValue = numericModel('accountAutoFreezeWindowSeconds')
  const accountAutoFreezeDurationSecondsValue = numericModel('accountAutoFreezeDurationSeconds')

  const minCodexDesktopVersionError = computed(() => versionError(form.minCodexDesktopVersion))
  const minCodexCliVersionError = computed(() => versionError(form.minCodexCliVersion))

  function versionError(value: string): string {
    const normalized = value.trim()
    return normalized && !isSemver(normalized) ? '请输入标准 SemVer，例如 0.152.0' : ''
  }

  function applySettings(data: Awaited<ReturnType<typeof getSettings>>) {
    savedRequestLocation.value = { ...data.requestLocation }
    form.requestLocationEnabled = data.requestLocationEnabled
    form.requestLocation = { ...data.requestLocation }
    form.refreshMarginSeconds = data.refreshMarginSeconds
    form.refreshConcurrency = data.refreshConcurrency
    form.maxConcurrentPerAccount = data.maxConcurrentPerAccount
    form.requestIntervalMs = data.requestIntervalMs
    form.maxWaitingPerKey = data.maxWaitingPerKey
    form.maxWaitingPerAccount = data.maxWaitingPerAccount
    form.concurrencyWaitTimeoutSeconds = data.concurrencyWaitTimeoutSeconds
    form.responsesMaxDecompressedBodyMiB = data.responsesMaxDecompressedBodyBytes / MIB

    form.rotationStrategy = data.rotationStrategy
    form.minCodexDesktopVersion = data.minCodexDesktopVersion ?? ''
    form.minCodexCliVersion = data.minCodexCliVersion ?? ''
    form.usageRetentionDays = data.usageRetentionDays
    form.opsEventRetentionDays = data.opsEventRetentionDays
    form.auditRetentionDays = data.auditRetentionDays
    form.accountAutoFreezeEnabled = data.accountAutoFreezeEnabled
    form.accountAutoFreezeThreshold = data.accountAutoFreezeThreshold
    form.accountAutoFreezeWindowSeconds = data.accountAutoFreezeWindowSeconds
    form.accountAutoFreezeDurationSeconds = data.accountAutoFreezeDurationSeconds
    form.accountAutoFreezeProbeEnabled = data.accountAutoFreezeProbeEnabled
    form.accountAutoFreezeProbeModel = data.accountAutoFreezeProbeModel ?? ''
    form.accountAutoFreezeAdaptiveConcurrency = data.accountAutoFreezeAdaptiveConcurrency
    mappings.value = Object.entries(data.modelMappings || {}).map(([requestedModel, upstreamModel]) => ({
      requestedModel,
      upstreamModel: String(upstreamModel),
    }))
  }

  async function loadSettings(silent = false) {
    loading.value = true
    error.value = ''
    try {
      applySettings(await getSettings({ silent }))
    }
    catch (cause: unknown) {
      error.value = errorMessage(cause)
    }
    finally {
      loading.value = false
    }
  }

  function addMapping() {
    mappings.value = [...mappings.value, { requestedModel: '', upstreamModel: '' }]
  }

  function updateMapping(index: number, key: 'requestedModel' | 'upstreamModel', value: string) {
    const rows = [...mappings.value]
    if (!rows[index])
      return
    rows[index] = { ...rows[index], [key]: value }
    mappings.value = rows
  }

  function removeMapping(index: number) {
    const rows = [...mappings.value]
    rows.splice(index, 1)
    mappings.value = rows
  }

  function mappingPayload() {
    const entries: Record<string, string> = {}
    for (const row of mappings.value) {
      const requested = row.requestedModel.trim()
      const upstream = row.upstreamModel.trim()
      if (!requested || !upstream)
        throw new Error('请完整填写模型映射')
      if (entries[requested])
        throw new Error(`存在重复的客户端模型：${requested}`)
      entries[requested] = upstream
    }
    return entries
  }

  async function saveSettings() {
    if (saving.value || loading.value || !savedRequestLocation.value)
      return
    const { refreshMarginSeconds, refreshConcurrency, maxConcurrentPerAccount, requestIntervalMs, rotationStrategy, maxWaitingPerKey, maxWaitingPerAccount, concurrencyWaitTimeoutSeconds, responsesMaxDecompressedBodyMiB, accountAutoFreezeThreshold, accountAutoFreezeWindowSeconds, accountAutoFreezeDurationSeconds } = form
    if (refreshMarginSeconds === null || refreshConcurrency === null || maxConcurrentPerAccount === null || requestIntervalMs === null || !rotationStrategy || maxWaitingPerKey === null || maxWaitingPerAccount === null || concurrencyWaitTimeoutSeconds === null) {
      toast.warning('请完整填写运行参数和调度策略')
      return
    }
    if (responsesMaxDecompressedBodyMiB === null || !Number.isInteger(responsesMaxDecompressedBodyMiB) || responsesMaxDecompressedBodyMiB < 1
      || !Number.isSafeInteger(responsesMaxDecompressedBodyMiB * MIB)) {
      toast.warning('Responses 解压上限应为有效的正整数（MiB）')
      return
    }
    if (![maxWaitingPerKey, maxWaitingPerAccount].every(value => Number.isInteger(value) && value >= 0 && value <= 1000)
      || !Number.isInteger(concurrencyWaitTimeoutSeconds) || concurrencyWaitTimeoutSeconds < 1 || concurrencyWaitTimeoutSeconds > 120) {
      toast.warning('最大排队数应为 0～1000 的整数，最长排队时间应为 1～120 秒的整数')
      return
    }
    if (minCodexDesktopVersionError.value || minCodexCliVersionError.value) {
      toast.warning('请修正客户端最低版本格式')
      return
    }
    // 关闭时保留已保存的自定义值，未完成的草稿不阻止停止覆盖。
    const requestLocation = form.requestLocationEnabled
      ? normalizeRequestLocation(form.requestLocation)
      : savedRequestLocation.value
    const locationError = requestLocationError(requestLocation)
    if (locationError) {
      toast.warning(locationError)
      return
    }
    if (accountAutoFreezeThreshold === null || accountAutoFreezeWindowSeconds === null || accountAutoFreezeDurationSeconds === null) {
      toast.warning('请完整填写账号自动冻结参数')
      return
    }
    if (!Number.isInteger(accountAutoFreezeThreshold) || accountAutoFreezeThreshold < 2 || accountAutoFreezeThreshold > 1000
      || !Number.isInteger(accountAutoFreezeWindowSeconds) || accountAutoFreezeWindowSeconds < 60 || accountAutoFreezeWindowSeconds > 3600
      || !Number.isInteger(accountAutoFreezeDurationSeconds) || accountAutoFreezeDurationSeconds < 300 || accountAutoFreezeDurationSeconds > 604800) {
      toast.warning('自动冻结阈值应为 2～1000，统计窗口为 60～3600 秒，冻结时长为 300～604800 秒')
      return
    }
    const probeModel = form.accountAutoFreezeProbeModel.trim()
    if (probeModel && (probeModel.length > 128 || probeModel !== probeModel.trim())) {
      toast.warning('探测模型名称不能超过 128 个字符')
      return
    }
    await saveAction.run(async () => {
      const result = await updateSettings({
        requestLocationEnabled: form.requestLocationEnabled,
        requestLocation,
        modelMappings: mappingPayload(),
        refreshMarginSeconds,
        refreshConcurrency,
        maxConcurrentPerAccount,
        requestIntervalMs,
        maxWaitingPerKey,
        maxWaitingPerAccount,
        concurrencyWaitTimeoutSeconds,
        responsesMaxDecompressedBodyBytes: responsesMaxDecompressedBodyMiB * MIB,
        rotationStrategy,
        minCodexDesktopVersion: form.minCodexDesktopVersion.trim() || null,
        minCodexCliVersion: form.minCodexCliVersion.trim() || null,
        usageRetentionDays: form.usageRetentionDays,
        opsEventRetentionDays: form.opsEventRetentionDays,
        auditRetentionDays: form.auditRetentionDays,
        accountAutoFreezeEnabled: form.accountAutoFreezeEnabled,
        accountAutoFreezeThreshold,
        accountAutoFreezeWindowSeconds,
        accountAutoFreezeDurationSeconds,
        accountAutoFreezeProbeEnabled: form.accountAutoFreezeProbeEnabled,
        accountAutoFreezeProbeModel: probeModel || null,
        accountAutoFreezeAdaptiveConcurrency: form.accountAutoFreezeAdaptiveConcurrency,
      })
      applySettings(result)
      toast.success('设置已保存')
    }, {
      onError: (cause) => {
        if (cause instanceof ApiError)
          void loadSettings(true)
      },
    })
  }

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
    maxWaitingPerKeyValue,
    maxWaitingPerAccountValue,
    concurrencyWaitTimeoutSecondsValue,
    responsesMaxDecompressedBodyMiBValue,
    accountAutoFreezeThresholdValue,
    accountAutoFreezeWindowSecondsValue,
    accountAutoFreezeDurationSecondsValue,
    minCodexDesktopVersionError,
    minCodexCliVersionError,
    saveSettings,
    loadSettings,
  }
}

function isSemver(value: string): boolean {
  if (value.length > 64 || value.startsWith('v'))
    return false

  const buildParts = value.split('+')
  if (buildParts.length > 2)
    return false
  const [versionAndPrerelease = '', build] = buildParts
  if (build !== undefined && !validIdentifiers(build, false))
    return false

  const prereleaseSeparator = versionAndPrerelease.indexOf('-')
  const core = prereleaseSeparator < 0
    ? versionAndPrerelease
    : versionAndPrerelease.slice(0, prereleaseSeparator)
  const prerelease = prereleaseSeparator < 0
    ? undefined
    : versionAndPrerelease.slice(prereleaseSeparator + 1)
  if (prerelease !== undefined && !validIdentifiers(prerelease, true))
    return false

  const coreParts = core.split('.')
  return coreParts.length === 3 && coreParts.every(validCoreNumericIdentifier)
}

function validIdentifiers(value: string, rejectNumericLeadingZeros: boolean): boolean {
  return Boolean(value) && value.split('.').every((identifier) => {
    if (!identifier || !/^[\da-z-]+$/i.test(identifier))
      return false
    return !rejectNumericLeadingZeros || !/^\d+$/.test(identifier) || validNumericIdentifier(identifier)
  })
}

function validNumericIdentifier(value: string): boolean {
  return /^(?:0|[1-9]\d*)$/.test(value)
}

function validCoreNumericIdentifier(value: string): boolean {
  return validNumericIdentifier(value) && BigInt(value) <= 18_446_744_073_709_551_615n
}
