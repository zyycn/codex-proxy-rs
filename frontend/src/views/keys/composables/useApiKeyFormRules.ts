import type { FormRule, FormRules } from '@codex-proxy/ui'
import type { ApiKeyFormValue } from './useApiKeyEditor'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

export function useApiKeyFormRules(form: () => ApiKeyFormValue) {
  const { t } = useI18n({ useScope: 'global' })

  function amountRule(label: string): FormRule {
    return {
      trigger: ['blur', 'change'],
      transform: value => String(value ?? '').trim(),
      pattern: /^\d{1,10}(?:\.\d{1,10})?$/,
      message: t('keys.editor.validation.amount', { label }),
    }
  }

  function integerRule(label: string): FormRule {
    return {
      trigger: ['blur', 'change'],
      type: 'integer',
      transform: Number,
      min: 0,
      max: Number.MAX_SAFE_INTEGER,
      message: t('keys.editor.validation.integer', { label }),
    }
  }

  return computed<FormRules<ApiKeyFormValue>>(() => ({
    groupIds: {
      type: 'array',
      validator: (_rule, value) => !form().peerSharing.enabled || (Array.isArray(value) && value.length > 0),
      message: t('keys.editor.peerGroupRequired'),
      trigger: ['change'],
    },
    name: {
      required: true,
      whitespace: true,
      message: t('keys.editor.enterApiKeyName'),
      trigger: ['blur', 'change'],
    },
    customKey: {
      trigger: ['blur', 'change'],
      pattern: /^[\x21-\x7E]+$/,
      message: t('keys.editor.invalidKey'),
    },
    dailyLimitUsd: amountRule(t('keys.editor.dailyLimit')),
    weeklyLimitUsd: amountRule(t('keys.editor.weeklyLimit')),
    maxConcurrency: integerRule(t('keys.editor.maximumConcurrency')),
    requestsPerMinute: integerRule(t('keys.editor.requestsPerMinute')),
  }))
}
