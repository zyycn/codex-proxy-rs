import type { FormRules } from '@codex-proxy/ui'
import type { ApiKeyAccountForm } from '../utils/upstreamApiKey'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

export function useAccountApiKeyFormRules(editing = false) {
  const { t } = useI18n({ useScope: 'global' })
  return computed<FormRules<{ apiKey: ApiKeyAccountForm }>>(() => ({
    'apiKey.protocol': {
      required: true,
      trigger: 'change',
      message: t('accounts.credentials.validation.protocolRequired'),
    },
    'apiKey.transport': {
      required: true,
      trigger: 'change',
      message: t('accounts.credentials.validation.transportRequired'),
    },
    'apiKey.base_url': [
      {
        required: true,
        whitespace: true,
        trigger: ['blur', 'change'],
        message: t('accounts.credentials.validation.urlRequired'),
      },
      {
        max: 2048,
        transform: value => value.trim(),
        trigger: ['blur', 'change'],
        message: t('accounts.credentials.validation.urlLength'),
      },
      {
        trigger: ['blur', 'change'],
        validator: (_rule, value) => {
          try {
            const url = new URL(value)
            return ['https:', 'http:'].includes(url.protocol) && !!url.hostname
              && !url.username && !url.password && !url.search && !url.hash
          }
          catch {
            return false
          }
        },
        message: t('accounts.credentials.validation.urlFormat'),
      },
    ],
    'apiKey.apiKey': [
      {
        required: !editing,
        trigger: ['blur', 'change'],
        message: t('accounts.credentials.apiKeyRequired'),
      },
      {
        type: 'string',
        pattern: /^[\x21-\x7E]+$/,
        max: 16384,
        trigger: ['blur', 'change'],
        message: t('accounts.credentials.validation.apiKeyFormat'),
      },
    ],
  }))
}
