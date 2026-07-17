import { delay } from 'es-toolkit'

export function errorMessage(error: unknown, fallback = '请求失败') {
  return error instanceof Error && error.message ? error.message : fallback
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
