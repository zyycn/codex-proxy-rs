import { WEB_BASE_PATH } from '@/utils/deployment'

export const API_BASE_URL = import.meta.env.DEV
  ? '/dev'
  : WEB_BASE_PATH.replace(/\/+$/, '')
export const API_TIMEOUT_MS = 120_000
