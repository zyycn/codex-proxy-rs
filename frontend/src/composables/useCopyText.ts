import { useClipboard } from '@vueuse/core'
import { t } from '@/i18n'

import { notification as ZNotification } from '@/utils/notification'

interface CopyTextOptions {
  successText?: string
  notifyWhenEmpty?: boolean
}

export function useCopyText() {
  // 普通 HTTP 页面没有 Clipboard API，启用 VueUse 内置的兼容复制路径。
  const { copy } = useClipboard({ legacy: true })

  return async function copyText(value: string, options: CopyTextOptions = {}) {
    if (!value) {
      if (options.notifyWhenEmpty) {
        ZNotification.error({
          message: t('common.errors.nothingToCopy'),
        })
      }
      return
    }
    try {
      await copy(value)
      ZNotification.success({ message: options.successText ?? t('common.feedback.copied') })
    }
    catch {
      ZNotification.error({
        message: t('common.errors.copy'),
      })
    }
  }
}
