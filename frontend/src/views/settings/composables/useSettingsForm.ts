import type { rotationOptions } from '../constants'
import type { AccountAffinity, CodexPrivacyPolicy, SmartSchedulingConfig } from '@/api'
import type { ProviderRequestProfiles, ProviderRequestProfileUpdates } from '@/api/modules/settings/profiles'
import { cloneDeep, isEqual } from 'es-toolkit'
import { computed, reactive, ref, shallowRef } from 'vue'
import { detectRequestLocation, getSettings, updateSettings } from '@/api'

import { ApiError } from '@/api/request'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { t } from '@/i18n'
import { normalizeRequestLocation, requestLocationError } from '@/utils/location'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage } from '@/utils/operation'

type RotationStrategy = (typeof rotationOptions.value)[number]['value']

const MIB = 1024 * 1024

export function useSettingsForm() {
  const loading = shallowRef(true)
  const saveAction = useAsyncAction()
  const saving = saveAction.loading
  const locationAction = useAsyncAction()
  const detectingLocation = locationAction.loading
  const error = shallowRef('')
  const mappings = ref<Array<{ requestedModel: string, upstreamModel: string }>>([])
  const smartSchedulingDefaults = shallowRef<SmartSchedulingConfig>()
  const form = reactive({
    configRevision: 0,
    smartScheduling: undefined as SmartSchedulingConfig | undefined,
    providerRequestProfiles: {} as ProviderRequestProfiles,
    codexPrivacyPolicy: { enabled: false, onError: 'skip_rule', rules: [] } as CodexPrivacyPolicy,
    requestLocationEnabled: false,
    requestLocation: {
      country: '',
      region: '',
      city: '',
      timezone: '',
    },
    refreshMarginSeconds: null as number | null,
    refreshConcurrency: null as number | null,
    maxConcurrentPerAccount: null as number | null,
    openaiGuardianReservedConcurrency: null as number | null,
    openaiAccountAffinity: '' as AccountAffinity | '',
    maxAccountRotations: null as number | null,
    openaiSessionAffinityTtlHours: null as number | null,
    maxWaitingPerKey: null as number | null,
    maxWaitingPerAccount: null as number | null,
    concurrencyWaitTimeoutSeconds: null as number | null,
    responsesMaxDecompressedBodyMiB: null as number | null,

    rotationStrategy: '' as RotationStrategy | '',
    minCodexDesktopVersion: '',
    minCodexCliVersion: '',
    usageRetentionDays: 31 as number | null,
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
    resetCreditLeadMinutes: 10 as number | null,
    resetCreditMaxWeeklyRemainingPercent: null as number | null,
  })

  function snapshot() {
    return cloneDeep({ form, mappings: mappings.value })
  }

  const saved = shallowRef<ReturnType<typeof snapshot>>()
  const hasChanges = computed(() => saved.value !== undefined
    && !isEqual({ form, mappings: mappings.value }, saved.value))

  function resetSettings() {
    if (!saved.value || saving.value || detectingLocation.value)
      return
    const initial = cloneDeep(saved.value)
    Object.assign(form, initial.form)
    mappings.value = initial.mappings
  }

  function numericModel(key: 'resetCreditMaxWeeklyRemainingPercent' | 'resetCreditLeadMinutes' | 'refreshMarginSeconds' | 'refreshConcurrency' | 'maxConcurrentPerAccount' | 'openaiGuardianReservedConcurrency' | 'maxAccountRotations' | 'openaiSessionAffinityTtlHours' | 'maxWaitingPerKey' | 'maxWaitingPerAccount' | 'concurrencyWaitTimeoutSeconds' | 'responsesMaxDecompressedBodyMiB' | 'usageRetentionDays' | 'accountAutoFreezeThreshold' | 'accountAutoFreezeWindowSeconds' | 'accountAutoFreezeDurationSeconds') {
    return computed({
      get: () => (form[key] === null
        ? ''
        : String(form[key])),
      set: (value: string) => {
        if (!value.trim()) {
          form[key] = null
          return
        }
        const parsed = Number(value)
        form[key] = Number.isFinite(parsed)
          ? parsed
          : null
      },
    })
  }

  const refreshMarginSecondsValue = numericModel('refreshMarginSeconds')
  const refreshConcurrencyValue = numericModel('refreshConcurrency')
  const maxConcurrentPerAccountValue = numericModel('maxConcurrentPerAccount')
  const openaiGuardianReservedConcurrencyValue = numericModel('openaiGuardianReservedConcurrency')
  const maxAccountRotationsValue = numericModel('maxAccountRotations')
  const openaiSessionAffinityTtlHoursValue = numericModel('openaiSessionAffinityTtlHours')
  const maxWaitingPerKeyValue = numericModel('maxWaitingPerKey')
  const maxWaitingPerAccountValue = numericModel('maxWaitingPerAccount')
  const responsesMaxDecompressedBodyMiBValue = numericModel('responsesMaxDecompressedBodyMiB')
  const resetCreditLeadMinutesValue = numericModel('resetCreditLeadMinutes')
  const resetCreditMaxWeeklyRemainingPercentValue = numericModel('resetCreditMaxWeeklyRemainingPercent')
  const usageRetentionDaysValue = numericModel('usageRetentionDays')
  const concurrencyWaitTimeoutSecondsValue = numericModel('concurrencyWaitTimeoutSeconds')
  const accountAutoFreezeThresholdValue = numericModel('accountAutoFreezeThreshold')
  const accountAutoFreezeWindowSecondsValue = numericModel('accountAutoFreezeWindowSeconds')
  const accountAutoFreezeDurationSecondsValue = numericModel('accountAutoFreezeDurationSeconds')

  const minCodexDesktopVersionError = computed(() => versionError(form.minCodexDesktopVersion))
  const minCodexCliVersionError = computed(() => versionError(form.minCodexCliVersion))

  function versionError(value: string): string {
    const normalized = value.trim()
    return normalized && !isSemver(normalized)
      ? t('settings.form.enterStandardSemverForExample')
      : ''
  }

  function applySettings(data: Awaited<ReturnType<typeof getSettings>>) {
    form.codexPrivacyPolicy = cloneDeep(data.codexPrivacyPolicy)
    form.configRevision = data.configRevision
    form.requestLocationEnabled = data.requestLocationEnabled
    form.requestLocation = { ...data.requestLocation }
    form.refreshMarginSeconds = data.refreshMarginSeconds
    form.refreshConcurrency = data.refreshConcurrency
    form.maxConcurrentPerAccount = data.maxConcurrentPerAccount
    form.openaiGuardianReservedConcurrency = data.openaiGuardianReservedConcurrency
    form.openaiAccountAffinity = data.openaiAccountAffinity
    form.maxAccountRotations = data.maxAccountRotations
    form.openaiSessionAffinityTtlHours = data.openaiSessionAffinityTtlHours
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
    form.resetCreditLeadMinutes = data.resetCreditLeadMinutes
    form.resetCreditMaxWeeklyRemainingPercent = data.resetCreditMaxWeeklyRemainingPercent
    mappings.value = Object.entries(data.modelMappings).map(([requestedModel, upstreamModel]) => ({
      requestedModel,
      upstreamModel,
    }))
    saved.value = snapshot()
  }

  async function loadSettings(silent = false) {
    if (detectingLocation.value)
      return
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

  async function detectLocation() {
    if (saving.value || loading.value || error.value)
      return
    await locationAction.run(async () => {
      const result = await detectRequestLocation()
      if (result.status === 'detected') {
        form.requestLocation = { ...result.location }
        form.requestLocationEnabled = true
        ZNotification.success({
          message: t('settings.form.exitLocationFilledIn'),
        })
      }
      else if (result.status === 'conflict') {
        ZNotification.warning({
          message: t('settings.form.ipv4AndIpv6ExportTimeZonesAre'),
        })
      }
      else {
        ZNotification.warning({
          message: result.status === 'failed'
            ? result.message
            : t('settings.form.unableToObtainExitLocation'),
        })
      }
    })
  }

  function addMapping() {
    mappings.value = [
      ...mappings.value,
      { requestedModel: '', upstreamModel: '' },
    ]
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
        throw new Error(t('settings.form.fillInModelMappingCompletely'))
      if (entries[requested])
        throw new Error(t('settings.form.duplicateClientModel', { requested }))
      entries[requested] = upstream
    }
    return entries
  }

  async function saveSettings() {
    const smartScheduling = form.smartScheduling
    if (!smartScheduling)
      return
    const savedSettings = saved.value
    if (saving.value || loading.value || detectingLocation.value || !savedSettings)
      return
    const { refreshMarginSeconds, refreshConcurrency, maxConcurrentPerAccount, openaiGuardianReservedConcurrency, openaiAccountAffinity, maxAccountRotations, openaiSessionAffinityTtlHours, rotationStrategy, maxWaitingPerKey, maxWaitingPerAccount, concurrencyWaitTimeoutSeconds, responsesMaxDecompressedBodyMiB, accountAutoFreezeThreshold, accountAutoFreezeWindowSeconds, accountAutoFreezeDurationSeconds } = form
    if (!openaiAccountAffinity || maxAccountRotations === null || openaiSessionAffinityTtlHours === null || refreshMarginSeconds === null || refreshConcurrency === null || maxConcurrentPerAccount === null || openaiGuardianReservedConcurrency === null || !rotationStrategy || maxWaitingPerKey === null || maxWaitingPerAccount === null || concurrencyWaitTimeoutSeconds === null) {
      ZNotification.warning({
        message: t('settings.form.requiredSettings'),
      })
      return
    }
    if (!Number.isInteger(openaiSessionAffinityTtlHours) || openaiSessionAffinityTtlHours < 1 || openaiSessionAffinityTtlHours > 720) {
      ZNotification.warning({
        message: t('settings.form.invalidAffinityDuration'),
      })
      return
    }
    if (!Number.isInteger(maxAccountRotations) || maxAccountRotations < 0 || maxAccountRotations > 31) {
      ZNotification.warning({
        message: t('settings.form.invalidSwitchLimit'),
      })
      return
    }
    if (!Number.isInteger(maxConcurrentPerAccount) || maxConcurrentPerAccount < 0 || maxConcurrentPerAccount > 4294967295) {
      ZNotification.warning({
        message: t('settings.form.invalidConcurrency'),
      })
      return
    }
    if (!Number.isInteger(openaiGuardianReservedConcurrency) || openaiGuardianReservedConcurrency < 0 || openaiGuardianReservedConcurrency > 4294967295) {
      ZNotification.warning({
        message: t('settings.form.invalidReviewConcurrency'),
      })
      return
    }
    if (responsesMaxDecompressedBodyMiB === null || !Number.isInteger(responsesMaxDecompressedBodyMiB) || responsesMaxDecompressedBodyMiB < 1
      || !Number.isSafeInteger(responsesMaxDecompressedBodyMiB * MIB)) {
      ZNotification.warning({
        message: t('settings.form.invalidBodyLimit'),
      })
      return
    }
    const resetCreditMaxWeeklyRemainingPercent = form.resetCreditMaxWeeklyRemainingPercent
    if (resetCreditMaxWeeklyRemainingPercent !== null && (!Number.isInteger(resetCreditMaxWeeklyRemainingPercent) || resetCreditMaxWeeklyRemainingPercent < 0 || resetCreditMaxWeeklyRemainingPercent > 100)) {
      ZNotification.warning({
        message: t('settings.form.invalidQuotaThreshold'),
      })
      return
    }
    const resetCreditLeadMinutes = form.resetCreditLeadMinutes
    if (resetCreditLeadMinutes === null || !Number.isInteger(resetCreditLeadMinutes) || resetCreditLeadMinutes < 1 || resetCreditLeadMinutes > 1440) {
      ZNotification.warning({
        message: t('settings.form.invalidResetLeadTime'),
      })
      return
    }
    const usageRetentionDays = form.usageRetentionDays
    if (usageRetentionDays === null || !Number.isInteger(usageRetentionDays) || usageRetentionDays < 1 || usageRetentionDays > 31) {
      ZNotification.warning({
        message: t('settings.form.invalidRetention'),
      })
      return
    }
    if (![maxWaitingPerKey, maxWaitingPerAccount].every(value => Number.isInteger(value) && value >= 0 && value <= 1000)
      || !Number.isInteger(concurrencyWaitTimeoutSeconds) || concurrencyWaitTimeoutSeconds < 1 || concurrencyWaitTimeoutSeconds > 120) {
      ZNotification.warning({
        message: t('settings.form.invalidQueueSettings'),
      })
      return
    }
    if (minCodexDesktopVersionError.value || minCodexCliVersionError.value) {
      ZNotification.warning({
        message: t('settings.form.invalidClientVersion'),
      })
      return
    }
    // 关闭时保留已保存的自定义值，未完成的草稿不阻止停止覆盖。
    const requestLocation = form.requestLocationEnabled
      ? normalizeRequestLocation(form.requestLocation)
      : savedSettings.form.requestLocation
    const locationError = requestLocationError(requestLocation)
    if (locationError) {
      ZNotification.warning({ message: locationError })
      return
    }
    if (accountAutoFreezeThreshold === null || accountAutoFreezeWindowSeconds === null || accountAutoFreezeDurationSeconds === null) {
      ZNotification.warning({
        message: t('settings.form.overloadSettingsRequired'),
      })
      return
    }
    if (!Number.isInteger(accountAutoFreezeThreshold) || accountAutoFreezeThreshold < 2 || accountAutoFreezeThreshold > 1000
      || !Number.isInteger(accountAutoFreezeWindowSeconds) || accountAutoFreezeWindowSeconds < 60 || accountAutoFreezeWindowSeconds > 3600
      || !Number.isInteger(accountAutoFreezeDurationSeconds) || accountAutoFreezeDurationSeconds < 300 || accountAutoFreezeDurationSeconds > 604800) {
      ZNotification.warning({
        message: t('settings.form.invalidOverloadSettings'),
      })
      return
    }
    const probeModel = form.accountAutoFreezeProbeModel.trim()
    if (probeModel.length > 128) {
      ZNotification.warning({
        message: t('settings.form.invalidProbeModel'),
      })
      return
    }
    const scheduleTime = form.accountWarmupScheduleTime.trim()
    const timeRegex = /^(?:[01]\d|2[0-3]):[0-5]\d(?:,(?:[01]\d|2[0-3]):[0-5]\d)*$/
    if (!scheduleTime || !timeRegex.test(scheduleTime)) {
      ZNotification.warning({
        message: t('settings.form.invalidWarmupTimes'),
      })
      return
    }
    const warmupModel = form.accountWarmupModel.trim()
    if (form.accountWarmupEnabled && !warmupModel) {
      ZNotification.warning({
        message: t('settings.form.warmupModelRequired'),
      })
      return
    }
    if (warmupModel.length > 128) {
      ZNotification.warning({
        message: t('settings.form.invalidWarmupModel'),
      })
      return
    }
    await saveAction.run(async () => {
      const result = await updateSettings({
        configRevision: savedSettings.form.configRevision,
        providerRequestProfiles: requestProfileUpdates(
          savedSettings.form.providerRequestProfiles,
          form.providerRequestProfiles,
        ),
        codexPrivacyPolicy: cloneDeep(form.codexPrivacyPolicy),
        requestLocationEnabled: form.requestLocationEnabled,
        requestLocation,
        modelMappings: mappingPayload(),
        refreshMarginSeconds,
        refreshConcurrency,
        maxConcurrentPerAccount,
        openaiGuardianReservedConcurrency,
        openaiAccountAffinity,
        maxAccountRotations,
        openaiSessionAffinityTtlHours,
        maxWaitingPerKey,
        maxWaitingPerAccount,
        concurrencyWaitTimeoutSeconds,
        responsesMaxDecompressedBodyBytes: responsesMaxDecompressedBodyMiB * MIB,
        rotationStrategy,
        smartScheduling: { ...smartScheduling },
        minCodexDesktopVersion: form.minCodexDesktopVersion.trim() || null,
        minCodexCliVersion: form.minCodexCliVersion.trim() || null,
        usageRetentionDays,
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
        resetCreditLeadMinutes,
        resetCreditMaxWeeklyRemainingPercent,
      })
      applySettings(result)
      ZNotification.success({
        message: t('settings.form.settingsSaved'),
      })
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
    detectingLocation,
    detectLocation,
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
    maxAccountRotationsValue,
    openaiSessionAffinityTtlHoursValue,
    maxWaitingPerKeyValue,
    maxWaitingPerAccountValue,
    concurrencyWaitTimeoutSecondsValue,
    responsesMaxDecompressedBodyMiBValue,
    usageRetentionDaysValue,
    resetCreditLeadMinutesValue,
    resetCreditMaxWeeklyRemainingPercentValue,
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
  for (const provider of new Set([
    ...Object.keys(previous),
    ...Object.keys(current),
  ])) {
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
