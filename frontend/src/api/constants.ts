export const API_BASE_URL = import.meta.env.DEV ? '/dev' : ''
const timeout = Number(import.meta.env.VITE_API_TIMEOUT_MS ?? 120_000)
if (!Number.isSafeInteger(timeout) || timeout <= 0 || timeout > 2_147_483_647) {
  throw new Error('VITE_API_TIMEOUT_MS must be a positive 32-bit integer')
}
export const API_TIMEOUT_MS = timeout
