export const API_BASE_URL = import.meta.env.DEV
  ? '/dev'
  : import.meta.env.BASE_URL.replace(/\/+$/, '')
export const API_TIMEOUT_MS = 120_000
