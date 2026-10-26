import { useClipboard } from '@vueuse/core'
import { t } from '@/i18n'

import { notification as ZNotification } from '@/utils/notification'
import { errorMessage } from '@/utils/operation'

interface CopyTextOptions {
  successText: string
  // 空值时的错误提示；缺省则静默返回。
  emptyErrorText?: string
  // 复制异常时是否透出异常内的 message（否则固定“复制失败”）。
  errorFromException?: boolean
}

// 剪贴板复制 + notification 反馈；空值与错误文案语义由调用方按站点配置。
export function useCopyText() {
  // 普通 HTTP 页面没有 Clipboard API，启用 VueUse 内置的兼容复制路径。
  const { copy } = useClipboard({ legacy: true })

  return async function copyText(value: string, options: CopyTextOptions) {
    if (!value) {
      if (options.emptyErrorText)
        ZNotification.error({ message: options.emptyErrorText })
      return
    }
    try {
      await copy(value)
      ZNotification.success({ message: options.successText })
    }
    catch (error: unknown) {
      ZNotification.error({
        message: options.errorFromException
          ? errorMessage(error, t('common.errors.copy'))
          : t('common.errors.copy'),
      })
    }
  }
}
