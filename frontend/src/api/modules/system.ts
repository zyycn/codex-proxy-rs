import request from '../request'
import { API_BASE_URL } from '../constants'

export const SYSTEM_UPDATE_EVENTS_URL = `${API_BASE_URL}/api/admin/system/update-events`

export function getSystemVersion(options: any = {}) {
  return request({
    url: '/api/admin/system/version',
    method: 'GET',
    ...(options.timeoutMs ? { timeout: options.timeoutMs } : {}),
  })
}

export function getSystemUpdateDetail(refresh = false) {
  return request({
    url: '/api/admin/system/update-detail',
    method: 'GET',
    params: { refresh },
  })
}

export function performSystemUpdate(targetVersion: any) {
  return request({
    url: '/api/admin/system/update',
    method: 'POST',
    data: { targetVersion },
  })
}

export function restartSystem() {
  return request({
    url: '/api/admin/system/restart',
    method: 'POST',
  })
}
