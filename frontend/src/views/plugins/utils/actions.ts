import { toast } from '@codex-proxy/ui'
import { ApiError } from '@/api/request'
import { errorMessage } from '@/utils/operation'

export interface PluginRefreshContext {
  refresh: (silent?: boolean, suppressErrors?: boolean) => Promise<void>
}

// 插件操作会聚合多个静默请求，由操作边界统一反馈一次错误。
export function notifyPluginError(title: string, error: unknown): void {
  if (error instanceof ApiError && error.kind === 'cancelled')
    return
  toast.error(errorMessage(error, title))
}
