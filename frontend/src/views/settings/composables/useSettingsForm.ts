import type { ProtectionDraft } from '../components/error-protection/form'
import type { rotationOptions } from '../constants'
import type { AccountAffinity, CodexPrivacyPolicy, SmartSchedulingConfig } from '@/api'
import type { ProviderRequestProfiles, ProviderRequestProfileUpdates } from '@/api/modules/settings/profiles'
import { cloneDeep, isEqual } from 'es-toolkit'
import { computed, reactive, ref, shallowRef } from 'vue'
import { detectRequestLocation, getSettings, updateSettings } from '@/api'

import { ApiError } from '@/api/request'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { t } from '@/i18n'
import { normalizeRequestLocation } from '@/utils/location'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage } from '@/utils/operation'
import { trimProtectionText } from '../components/error-protection/form'

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
    keyQueueTimeoutSeconds: null as number | null,
    accountQueueTimeoutSeconds: null as number | null,
    switchAccountOnQueueFailure: false,
    maxRetryAfterSeconds: null as number | null,
    responsesMaxDecompressedBodyMiB: null as number | null,

    rotationStrategy: '' as RotationStrategy | '',
    minCodexDesktopVersion: '',
    minCodexCliVersion: '',
    usageRetentionDays: 31 as number | null,
    opsEventRetentionDays: 30,
    auditRetentionDays: 90,

    accountErrorProtection: {
      enabled: false,
      rules: [],
      threshold: 12,
      windowSeconds: 600,
      durationSeconds: 7200,
      probeEnabled: true,
      probeModel: '',
    } as ProtectionDraft,
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

  function numericModel(key: 'resetCreditMaxWeeklyRemainingPercent' | 'resetCreditLeadMinutes' | 'refreshMarginSeconds' | 'refreshConcurrency' | 'maxConcurrentPerAccount' | 'openaiGuardianReservedConcurrency' | 'maxAccountRotations' | 'openaiSessionAffinityTtlHours' | 'maxWaitingPerKey' | 'maxWaitingPerAccount' | 'keyQueueTimeoutSeconds' | 'accountQueueTimeoutSeconds' | 'maxRetryAfterSeconds' | 'responsesMaxDecompressedBodyMiB' | 'usageRetentionDays') {
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
  const keyQueueTimeoutSecondsValue = numericModel('keyQueueTimeoutSeconds')
  const accountQueueTimeoutSecondsValue = numericModel('accountQueueTimeoutSeconds')
  const maxRetryAfterSecondsValue = numericModel('maxRetryAfterSeconds')

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
    form.keyQueueTimeoutSeconds = data.keyQueueTimeoutSeconds
    form.accountQueueTimeoutSeconds = data.accountQueueTimeoutSeconds
    form.switchAccountOnQueueFailure = data.switchAccountOnQueueFailure
    form.maxRetryAfterSeconds = data.maxRetryAfterSeconds
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
    form.accountErrorProtection = {
      ...cloneDeep(data.accountErrorProtection),
      probeModel: data.accountErrorProtection.probeModel ?? '',
    }
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

  async function saveSettings() {
    const smartScheduling = form.smartScheduling
    if (!smartScheduling)
      return
    const savedSettings = saved.value
    if (saving.value || loading.value || detectingLocation.value || !savedSettings)
      return
    const { refreshMarginSeconds, refreshConcurrency, maxConcurrentPerAccount, openaiGuardianReservedConcurrency, openaiAccountAffinity, maxAccountRotations, openaiSessionAffinityTtlHours, rotationStrategy, maxWaitingPerKey, maxWaitingPerAccount, keyQueueTimeoutSeconds, accountQueueTimeoutSeconds, maxRetryAfterSeconds, responsesMaxDecompressedBodyMiB } = form
    const { resetCreditMaxWeeklyRemainingPercent, resetCreditLeadMinutes, usageRetentionDays } = form
    // 关闭时保留已保存的自定义值，未完成的草稿不阻止停止覆盖。
    const requestLocation = form.requestLocationEnabled
      ? normalizeRequestLocation(form.requestLocation)
      : savedSettings.form.requestLocation
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
        modelMappings: Object.fromEntries(mappings.value.map(row => [
          row.requestedModel.trim(),
          row.upstreamModel.trim(),
        ])),
        refreshMarginSeconds: refreshMarginSeconds!,
        refreshConcurrency: refreshConcurrency!,
        maxConcurrentPerAccount: maxConcurrentPerAccount!,
        openaiGuardianReservedConcurrency: openaiGuardianReservedConcurrency!,
        openaiAccountAffinity: openaiAccountAffinity as AccountAffinity,
        maxAccountRotations: maxAccountRotations!,
        openaiSessionAffinityTtlHours: openaiSessionAffinityTtlHours!,
        maxWaitingPerKey: maxWaitingPerKey!,
        maxWaitingPerAccount: maxWaitingPerAccount!,
        keyQueueTimeoutSeconds: keyQueueTimeoutSeconds!,
        accountQueueTimeoutSeconds: accountQueueTimeoutSeconds!,
        switchAccountOnQueueFailure: form.switchAccountOnQueueFailure,
        maxRetryAfterSeconds: maxRetryAfterSeconds!,
        responsesMaxDecompressedBodyBytes: responsesMaxDecompressedBodyMiB! * MIB,
        rotationStrategy: rotationStrategy as RotationStrategy,
        smartScheduling: { ...smartScheduling },
        minCodexDesktopVersion: form.minCodexDesktopVersion.trim() || null,
        minCodexCliVersion: form.minCodexCliVersion.trim() || null,
        usageRetentionDays: usageRetentionDays!,
        opsEventRetentionDays: form.opsEventRetentionDays,
        auditRetentionDays: form.auditRetentionDays,
        accountErrorProtection: {
          ...cloneDeep(form.accountErrorProtection),
          threshold: form.accountErrorProtection.threshold!,
          windowSeconds: form.accountErrorProtection.windowSeconds!,
          durationSeconds: form.accountErrorProtection.durationSeconds!,
          probeModel: trimProtectionText(form.accountErrorProtection.probeModel) || null,
        },
        accountWarmupEnabled: form.accountWarmupEnabled,
        accountWarmupScheduleTime: form.accountWarmupScheduleTime.trim(),
        accountWarmupModel: form.accountWarmupModel.trim() || null,
        resetCreditLeadMinutes: resetCreditLeadMinutes!,
        resetCreditMaxWeeklyRemainingPercent,
      })
      applySettings(result)
      ZNotification.success({
        message: t('settings.form.settingsSaved'),
      })
    }, {
      onError: (cause) => {
        // 明确拒绝的请求保留草稿，执行结果未知时仍回读设置以同步已提交的版本
        if (cause instanceof ApiError && cause.status !== 409
          && (cause.kind !== 'api' || cause.status < 400 || cause.status >= 500)) {
          void loadSettings(true)
        }
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
    keyQueueTimeoutSecondsValue,
    accountQueueTimeoutSecondsValue,
    maxRetryAfterSecondsValue,
    responsesMaxDecompressedBodyMiBValue,
    usageRetentionDaysValue,
    resetCreditLeadMinutesValue,
    resetCreditMaxWeeklyRemainingPercentValue,
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
