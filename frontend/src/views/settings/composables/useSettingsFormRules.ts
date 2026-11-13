import type { FormRule, FormRules } from '@codex-proxy/ui'
import type { useSettingsForm } from './useSettingsForm'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { protectionPolicyError, trimProtectionText } from '../components/error-protection/form'
import { isSemver } from '../utils/version'

type SettingsForm = ReturnType<typeof useSettingsForm>['form']

export function useSettingsFormRules(form: () => SettingsForm) {
  const { t } = useI18n({ useScope: 'global' })
  const integer = (min: number, max: number | undefined, message: string): FormRule => ({
    type: 'integer',
    required: true,
    min,
    max,
    message,
    trigger: ['blur', 'change'],
  })
  const version = (): FormRule => ({
    transform: value => value.trim(),
    validator: (_rule, value) => !value || isSemver(value),
    message: t('settings.form.enterStandardSemverForExample'),
    trigger: ['blur', 'change'],
  })

  return computed<FormRules<SettingsForm>>(() => ({
    'refreshMarginSeconds': integer(0, Number.MAX_SAFE_INTEGER, t('settings.form.invalidRefreshSettings')),
    'refreshConcurrency': integer(0, 4_294_967_295, t('settings.form.invalidRefreshSettings')),
    'maxConcurrentPerAccount': integer(0, 4_294_967_295, t('settings.form.invalidConcurrency')),
    'openaiGuardianReservedConcurrency': integer(0, 4_294_967_295, t('settings.form.invalidReviewConcurrency')),
    'openaiAccountAffinity': {
      required: true,
      trigger: 'change',
      message: t('settings.form.requiredSettings'),
    },
    'maxAccountRotations': integer(0, 31, t('settings.form.invalidSwitchLimit')),
    'openaiSessionAffinityTtlHours': integer(1, 720, t('settings.form.invalidAffinityDuration')),
    'maxWaitingPerKey': integer(0, 1000, t('settings.form.invalidQueueSettings')),
    'maxWaitingPerAccount': integer(0, 1000, t('settings.form.invalidQueueSettings')),
    'keyQueueTimeoutSeconds': integer(1, 120, t('settings.form.invalidQueueSettings')),
    'accountQueueTimeoutSeconds': integer(1, 120, t('settings.form.invalidQueueSettings')),
    'maxRetryAfterSeconds': integer(0, 4_294_967_295, t('settings.form.invalidRetryAfterLimit')),
    'responsesMaxDecompressedBodyMiB': integer(1, Math.floor(Number.MAX_SAFE_INTEGER / 1024 / 1024), t('settings.form.invalidBodyLimit')),
    'resetCreditLeadMinutes': integer(1, 1440, t('settings.form.invalidResetLeadTime')),
    'resetCreditMaxWeeklyRemainingPercent': {
      ...integer(0, 100, t('settings.form.invalidQuotaThreshold')),
      required: false,
    },
    'usageRetentionDays': integer(1, undefined, t('settings.form.invalidRetention')),
    'minCodexDesktopVersion': version(),
    'minCodexCliVersion': version(),
    'accountWarmupCron': [
      {
        validator: (_rule, expressions: string[]) => (!form().accountWarmupEnabled || expressions.length > 0)
          && expressions.every(expression => /^[^\s;]+(?: +[^\s;]+){4}$/u.test(expression)),
        message: t('settings.form.invalidWarmupCron'),
        trigger: ['blur', 'change'],
      },
      {
        validator: (_rule, expressions: string[]) => new TextEncoder().encode(expressions.join(';')).length <= 255,
        message: t('settings.form.warmupCronTooLong'),
        trigger: ['blur', 'change'],
      },
    ],
    'accountWarmupModel': [
      {
        required: form().accountWarmupEnabled,
        whitespace: true,
        trigger: ['blur', 'change'],
        message: t('settings.form.warmupModelRequired'),
      },
      {
        max: 128,
        transform: value => value.trim(),
        trigger: ['blur', 'change'],
        message: t('settings.form.invalidWarmupModel'),
      },
    ],
    'accountErrorProtection.threshold': integer(2, 1000, t('settings.form.invalidProtectionSettings')),
    'accountErrorProtection.windowSeconds': integer(60, 3600, t('settings.form.invalidProtectionSettings')),
    'accountErrorProtection.durationSeconds': integer(300, 604800, t('settings.form.invalidProtectionSettings')),
    'accountErrorProtection.probeModel': [
      {
        pattern: /^\P{Cc}*$/u,
        trigger: ['blur', 'change'],
        message: t('settings.form.invalidProbeModel'),
      },
      {
        max: 128,
        transform: trimProtectionText,
        trigger: ['blur', 'change'],
        message: t('settings.form.invalidProbeModel'),
      },
    ],
    'accountErrorProtection.rules': {
      // 子字段保留输入校验，整组完整性只在提交时检查
      trigger: [],
      validator: () => {
        const error = protectionPolicyError(form().accountErrorProtection)
        return error
          ? new Error(error)
          : true
      },
    },
  }))
}
