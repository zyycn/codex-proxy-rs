import type { AccountModelAccess } from '@/api'

export function accountModelAccessError(value: AccountModelAccess | undefined): string | undefined {
  if (!value || value.mode === 'all')
    return undefined
  if (value.models.length === 0)
    return '请至少选择或添加一个模型'
  if (value.models.length > 256)
    return '最多配置 256 个模型'
  if (value.models.some(id => accountModelIdError(id)))
    return '请使用有效的精确模型 ID，不支持通配符'
}

export function accountModelIdError(id: string): string | undefined {
  if (!id || id.trim() !== id || id.startsWith('__') || id.includes('*') || /\p{Cc}/u.test(id) || new TextEncoder().encode(id).length > 256)
    return '请输入有效的精确模型 ID，最多 256 字节，不支持通配符'
}
