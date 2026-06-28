import { delay } from 'es-toolkit'

export async function withMinimumDuration<T>(
  task: Promise<T> | (() => Promise<T>),
  minimumMs = 1000,
): Promise<T> {
  const startedAt = Date.now()
  try {
    return await (typeof task === 'function' ? task() : task)
  } finally {
    const remaining = minimumMs - (Date.now() - startedAt)
    if (remaining > 0) {
      await delay(remaining)
    }
  }
}
