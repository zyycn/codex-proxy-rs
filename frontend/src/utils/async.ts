import { delay } from 'es-toolkit'

export function errorMessage(error: unknown, fallback = '请求失败') {
  const message = error instanceof Error
    ? error.message
    : error && typeof error === 'object' && 'message' in error
      ? (error as { message?: unknown }).message
      : undefined
  return typeof message === 'string' && message ? message : fallback
}

export async function withMinimumDuration<T>(
  task: Promise<T> | (() => Promise<T>),
  minimumMs = 1000,
): Promise<T> {
  const startedAt = Date.now()
  try {
    return await (typeof task === 'function' ? task() : task)
  }
  finally {
    const remaining = minimumMs - (Date.now() - startedAt)
    if (remaining > 0) {
      await delay(remaining)
    }
  }
}
