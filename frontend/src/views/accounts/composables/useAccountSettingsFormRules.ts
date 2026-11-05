import type { FormRules } from '@codex-proxy/ui'
import type { AccountModelAccess } from '@/api'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { accountNameValid } from '../utils/accountName'
import { accountModelAccessError } from '../utils/modelAccess'

interface AccountSettingsFormValue {
  name?: string
  concurrencyLimit: string
  weight: string
  modelAccess?: AccountModelAccess
  proxyMode: string
  proxyId: string
}

export function useAccountSettingsFormRules(
  form: () => AccountSettingsFormValue,
  options: { editedFields?: () => ReadonlySet<string>, savedName?: () => string } = {},
) {
  const { editedFields, savedName } = options
  const { t } = useI18n({ useScope: 'global' })
  return computed<FormRules<AccountSettingsFormValue>>(() => ({
    name: {
      trigger: ['blur', 'change'],
      validator: () => accountNameValid(form().name ?? '', savedName?.()),
      message: t('accounts.editor.nameInvalid'),
    },
    concurrencyLimit: {
      type: 'integer',
      trigger: ['blur', 'change'],
      transform: value => (editedFields && !editedFields().has('concurrencyLimit')) || !String(value).trim()
        ? undefined
        : Number(value),
      min: 1,
      max: 4_294_967_295,
      message: t('accounts.scheduling.invalidConcurrency', { value: 4_294_967_295 }),
    },
    weight: {
      type: 'integer',
      required: true,
      trigger: ['blur', 'change'],
      transform: value => editedFields && !editedFields().has('weight')
        ? 1
        : String(value ?? '').trim()
          ? Number(value)
          : undefined,
      min: 1,
      max: 100,
      message: t('accounts.scheduling.weightMustBeIntegerFromTo'),
    },
    modelAccess: {
      // 模式切换允许暂时为空，完整配置在提交时校验
      trigger: [],
      validator: () => {
        const error = accountModelAccessError(form().modelAccess)
        return error
          ? new Error(error)
          : true
      },
    },
    proxyId: {
      required: form().proxyMode === 'proxy',
      whitespace: true,
      trigger: 'change',
      message: t('accounts.batchEdit.proxyRequired'),
    },
  }))
}
