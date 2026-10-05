import type { rotationOptions } from '../constants'
import type { SmartSchedulingConfig } from '@/api'
import type { ProviderRequestProfiles, ProviderRequestProfileUpdates } from '@/api/modules/client-profiles'
import { toast } from '@codex-proxy/ui'
import { cloneDeep, isEqual } from 'es-toolkit'

import { computed, reactive, ref, shallowRef } from 'vue'
import { getSettings, updateSettings } from '@/api'
import { ApiError } from '@/api/request'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { normalizeRequestLocation, requestLocationError } from '@/utils/location'
import { errorMessage } from '@/utils/operation'

type RotationStrategy = (typeof rotationOptions)[number]['value']

const MIB = 1024 * 1024

export function useSettingsForm() {
  const loading = shallowRef(true)
  const saveAction = useAsyncAction()
  const saving = saveAction.loading
  const error = shallowRef('')
  const mappings = ref<Array<{ requestedModel: string, upstreamModel: string }>>([])
  const smartSchedulingDefaults = shallowRef<SmartSchedulingConfig>()
  const form = reactive({
    configRevision: 0,
    smartScheduling: undefined as SmartSchedulingConfig | undefined,
    providerRequestProfiles: {} as ProviderRequestProfiles,
    requestLocationEnabled: false,
    requestLocation: { country: '', region: '', city: '', timezone: '' },
    refreshMarginSeconds: null as number | null,
    refreshConcurrency: null as number | null,
    maxConcurrentPerAccount: null as number | null,
    openaiGuardianReservedConcurrency: null as number | null,
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
    accountWarmupEnabled: false,
    accountWarmupScheduleTime: '08:00',
    accountWarmupModel: '',
  })

  function snapshot() {
    return cloneDeep({ form, mappings: mappings.value })
  }

  const saved = shallowRef<ReturnType<typeof snapshot>>()
  const hasChanges = computed(() => saved.value !== undefined
    && !isEqual({ form, mappings: mappings.value }, saved.value))

  function resetSettings() {
    if (!saved.value || saving.value)
      return
    const initial = cloneDeep(saved.value)
    Object.assign(form, initial.form)
    mappings.value = initial.mappings
  }

  function numericModel(key: 'refreshMarginSeconds' | 'refreshConcurrency' | 'maxConcurrentPerAccount' | 'openaiGuardianReservedConcurrency' | 'requestIntervalMs' | 'maxWaitingPerKey' | 'maxWaitingPerAccount' | 'concurrencyWaitTimeoutSeconds' | 'responsesMaxDecompressedBodyMiB' | 'accountAutoFreezeThreshold' | 'accountAutoFreezeWindowSeconds' | 'accountAutoFreezeDurationSeconds') {
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
  const openaiGuardianReservedConcurrencyValue = numericModel('openaiGuardianReservedConcurrency')
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
    form.configRevision = data.configRevision
    form.requestLocationEnabled = data.requestLocationEnabled
    form.requestLocation = { ...data.requestLocation }
    form.refreshMarginSeconds = data.refreshMarginSeconds
    form.refreshConcurrency = data.refreshConcurrency
    form.maxConcurrentPerAccount = data.maxConcurrentPerAccount
    form.openaiGuardianReservedConcurrency = data.openaiGuardianReservedConcurrency
    form.requestIntervalMs = data.requestIntervalMs
    form.maxWaitingPerKey = data.maxWaitingPerKey
    form.maxWaitingPerAccount = data.maxWaitingPerAccount
    form.concurrencyWaitTimeoutSeconds = data.concurrencyWaitTimeoutSeconds
    form.responsesMaxDecompressedBodyMiB = data.responsesMaxDecompressedBodyBytes / MIB

    form.smartScheduling = { ...data.smartScheduling }
    smartSchedulingDefaults.value = { ...data.smartSchedulingDefaults }
    form.rotationStrategy = data.rotationStrategy
    form.minCodexDesktopVersion = data.minCodexDesktopVersion ?? ''
    form.providerRequestProfiles = cloneDeep(data.providerRequestProfiles)
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
    form.accountWarmupEnabled = data.accountWarmupEnabled
    form.accountWarmupScheduleTime = data.accountWarmupScheduleTime ?? '08:00'
    form.accountWarmupModel = data.accountWarmupModel ?? ''
    mappings.value = Object.entries(data.modelMappings).map(([requestedModel, upstreamModel]) => ({
      requestedModel,
      upstreamModel,
    }))
    saved.value = snapshot()
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
    const smartScheduling = form.smartScheduling
    if (!smartScheduling)
      return
    const savedSettings = saved.value
    if (saving.value || loading.value || !savedSettings)
      return
    const { refreshMarginSeconds, refreshConcurrency, maxConcurrentPerAccount, openaiGuardianReservedConcurrency, requestIntervalMs, rotationStrategy, maxWaitingPerKey, maxWaitingPerAccount, concurrencyWaitTimeoutSeconds, responsesMaxDecompressedBodyMiB, accountAutoFreezeThreshold, accountAutoFreezeWindowSeconds, accountAutoFreezeDurationSeconds } = form
    if (refreshMarginSeconds === null || refreshConcurrency === null || maxConcurrentPerAccount === null || openaiGuardianReservedConcurrency === null || requestIntervalMs === null || !rotationStrategy || maxWaitingPerKey === null || maxWaitingPerAccount === null || concurrencyWaitTimeoutSeconds === null) {
      toast.warning('请完整填写并发、队列、凭据刷新参数和调度策略')
      return
    }
    if (!Number.isInteger(maxConcurrentPerAccount) || maxConcurrentPerAccount < 0 || maxConcurrentPerAccount > 4294967295) {
      toast.warning('默认账号并发上限应为 0～4294967295 的整数，0 表示不限制')
      return
    }
    if (!Number.isInteger(openaiGuardianReservedConcurrency) || openaiGuardianReservedConcurrency < 0 || openaiGuardianReservedConcurrency > 4294967295) {
      toast.warning('自动审批预留并发应为 0～4294967295 的整数，0 表示关闭')
      return
    }
    if (responsesMaxDecompressedBodyMiB === null || !Number.isInteger(responsesMaxDecompressedBodyMiB) || responsesMaxDecompressedBodyMiB < 1
      || !Number.isSafeInteger(responsesMaxDecompressedBodyMiB * MIB)) {
      toast.warning('Responses 解压上限应为有效的正整数（MiB）')
      return
    }
    if (![maxWaitingPerKey, maxWaitingPerAccount].every(value => Number.isInteger(value) && value >= 0 && value <= 1000)
      || !Number.isInteger(concurrencyWaitTimeoutSeconds) || concurrencyWaitTimeoutSeconds < 1 || concurrencyWaitTimeoutSeconds > 120) {
      toast.warning('队列容量应为 0～1000 的整数，排队超时应为 1～120 秒的整数')
      return
    }
    if (minCodexDesktopVersionError.value || minCodexCliVersionError.value) {
      toast.warning('请修正客户端最低版本格式')
      return
    }
    // 关闭时保留已保存的自定义值，未完成的草稿不阻止停止覆盖。
    const requestLocation = form.requestLocationEnabled
      ? normalizeRequestLocation(form.requestLocation)
      : savedSettings.form.requestLocation
    const locationError = requestLocationError(requestLocation)
    if (locationError) {
      toast.warning(locationError)
      return
    }
    if (accountAutoFreezeThreshold === null || accountAutoFreezeWindowSeconds === null || accountAutoFreezeDurationSeconds === null) {
      toast.warning('请完整填写过载保护参数')
      return
    }
    if (!Number.isInteger(accountAutoFreezeThreshold) || accountAutoFreezeThreshold < 2 || accountAutoFreezeThreshold > 1000
      || !Number.isInteger(accountAutoFreezeWindowSeconds) || accountAutoFreezeWindowSeconds < 60 || accountAutoFreezeWindowSeconds > 3600
      || !Number.isInteger(accountAutoFreezeDurationSeconds) || accountAutoFreezeDurationSeconds < 300 || accountAutoFreezeDurationSeconds > 604800) {
      toast.warning('失败次数阈值应为 2～1000，统计窗口为 60～3600 秒，冷却时长为 300～604800 秒')
      return
    }
    const probeModel = form.accountAutoFreezeProbeModel.trim()
    if (probeModel.length > 128) {
      toast.warning('探测模型名称不能超过 128 个字符')
      return
    }
    const scheduleTime = form.accountWarmupScheduleTime.trim()
    const timeRegex = /^(?:[01]\d|2[0-3]):[0-5]\d(?:,(?:[01]\d|2[0-3]):[0-5]\d)*$/
    if (!scheduleTime || !timeRegex.test(scheduleTime)) {
      toast.warning('预激活时间格式无效，请输入 HH:MM 格式（如 08:00 或 08:00,13:00）')
      return
    }
    const warmupModel = form.accountWarmupModel.trim()
    if (form.accountWarmupEnabled && !warmupModel) {
      toast.warning('启用预激活时请选择模型')
      return
    }
    if (warmupModel.length > 128) {
      toast.warning('预激活模型名称不能超过 128 个字符')
      return
    }
    await saveAction.run(async () => {
      const result = await updateSettings({
        configRevision: savedSettings.form.configRevision,
        providerRequestProfiles: requestProfileUpdates(
          savedSettings.form.providerRequestProfiles,
          form.providerRequestProfiles,
        ),
        requestLocationEnabled: form.requestLocationEnabled,
        requestLocation,
        modelMappings: mappingPayload(),
        refreshMarginSeconds,
        refreshConcurrency,
        maxConcurrentPerAccount,
        openaiGuardianReservedConcurrency,
        requestIntervalMs,
        maxWaitingPerKey,
        maxWaitingPerAccount,
        concurrencyWaitTimeoutSeconds,
        responsesMaxDecompressedBodyBytes: responsesMaxDecompressedBodyMiB * MIB,
        rotationStrategy,
        smartScheduling: { ...smartScheduling },
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
        accountWarmupEnabled: form.accountWarmupEnabled,
        accountWarmupScheduleTime: scheduleTime,
        accountWarmupModel: warmupModel || null,
      })
      applySettings(result)
      toast.success('设置已保存')
    }, {
      onError: (cause) => {
        if (cause instanceof ApiError && cause.status !== 409)
          void loadSettings(true)
      },
    })
  }

  return {
    loading,
    saving,
    hasChanges,
    resetSettings,
    error,
    form,
    smartSchedulingDefaults,
    mappings,
    addMapping,
    updateMapping,
    removeMapping,
    refreshMarginSecondsValue,
    refreshConcurrencyValue,
    maxConcurrentPerAccountValue,
    openaiGuardianReservedConcurrencyValue,
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

function requestProfileUpdates(
  previous: ProviderRequestProfiles,
  current: ProviderRequestProfiles,
): ProviderRequestProfileUpdates {
  const updates: ProviderRequestProfileUpdates = {}
  const clonedCurrent = cloneDeep(current)
  for (const provider of new Set([...Object.keys(previous), ...Object.keys(current)])) {
    if (isEqual(previous[provider], current[provider]))
      continue
    updates[provider] = clonedCurrent[provider] ?? null
  }
  return updates
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
