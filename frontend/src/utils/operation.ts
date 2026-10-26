import { isAxiosError } from 'axios'
import { delay } from 'es-toolkit'
import { t } from '@/i18n'

export function errorMessage(error: unknown, fallback = t('common.errors.request')) {
  if (isAxiosError(error))
    return fallback
  const message = error instanceof Error
    ? error.message
    : error && typeof error === 'object' && 'message' in error
      ? (error as { message?: unknown }).message
      : undefined
  return typeof message === 'string' && message
    ? message
    : fallback
}

export async function withMinimumDuration<T>(
  task: Promise<T> | (() => Promise<T>),
  minimumMs = 1000,
): Promise<T> {
  const startedAt = Date.now()
  try {
    return await (typeof task === 'function'
      ? task()
      : task)
  }
  finally {
    const remaining = minimumMs - (Date.now() - startedAt)
    if (remaining > 0) {
      await delay(remaining)
    }
  }
}

export function generateRequestId() {
  // 普通 HTTP 管理端没有 randomUUID；getRandomValues 仍可生成密码学安全的 UUIDv4。
  const bytes = globalThis.crypto.getRandomValues(new Uint8Array(16))
  bytes[6] = (bytes[6] & 0x0F) | 0x40
  bytes[8] = (bytes[8] & 0x3F) | 0x80
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}
