import { useClipboard } from '@vueuse/core'

import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'

interface CopyTextOptions {
  successText: string
  // 空值时的错误提示；缺省则静默返回。
  emptyErrorText?: string
  // 复制异常时是否透出异常内的 message（否则固定“复制失败”）。
  errorFromException?: boolean
}

// 剪贴板复制 + toast 反馈；空值与错误文案语义由调用方按站点配置。
export function useCopyText() {
  const { copy } = useClipboard()

  return async function copyText(value: string, options: CopyTextOptions) {
    if (!value) {
      if (options.emptyErrorText)
        toast.error(options.emptyErrorText)
      return
    }
    try {
      await copy(value)
      toast.success(options.successText)
    }
    catch (error: unknown) {
      toast.error(options.errorFromException ? errorMessage(error, '复制失败') : '复制失败')
    }
  }
}
