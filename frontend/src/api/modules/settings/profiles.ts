import type { RequestOptions } from '../../request'
import request from '../../request'

export interface RequestProfilePreview {
  source: 'global' | 'override'
  userAgent: string
  versionSource: 'official' | 'custom'
  verifiedAt: string | null
  verifiedAtDisplay: string | null
  checkedAt: string | null
  checkedAtDisplay: string | null
  error: string | null
  versionLag?: number | null
}

export function getRequestProfileOptions<Options>(provider: string, options?: RequestOptions) {
  return request<Options>({
    url: `/api/admin/settings/client-profiles/${encodeURIComponent(provider)}`,
    method: 'GET',
    silent: true,
    ...options,
  })
}

export function previewRequestProfile<Preview>(provider: string, configuration: object | null, options?: RequestOptions) {
  return request<Preview>({
    url: `/api/admin/settings/client-profiles/${encodeURIComponent(provider)}/preview`,
    method: 'POST',
    data: { configuration },
    silent: true,
    ...options,
  })
}
