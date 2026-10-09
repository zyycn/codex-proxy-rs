import type { RequestOptions } from '../../request'
import request from '../../request'

export type PrivacyScope = 'turn_metadata' | 'desktop_git_context' | 'environment_text' | 'request_body' | 'request_header'
export type PrivacyAction = 'regex_replace' | 'set_value' | 'rename_key' | 'remove_field'
export type JsonValue = unknown

export interface PrivacyRule {
  id: string
  name: string
  enabled: boolean
  scope: PrivacyScope
  selector: string
  action: PrivacyAction
  pattern: string | null
  replacement: string
  value: JsonValue
  replaceAll: boolean
  caseInsensitive: boolean
  multiLine: boolean
}

export interface CodexPrivacyPolicy {
  enabled: boolean
  onError: 'skip_rule' | 'reject_request'
  rules: PrivacyRule[]
}

export interface PrivacySample {
  body: JsonValue
  headers: Record<string, string[]>
  turnMetadata: string | null
}

export interface PrivacyPreviewResult extends PrivacySample {
  outcomes: { ruleId: string, matches: number, status: string, reason: string | null }[]
}

export function previewPrivacyPolicy(data: PrivacySample & { policy: CodexPrivacyPolicy }, options: RequestOptions = {}) {
  return request<PrivacyPreviewResult>({
    url: '/api/admin/settings/privacy/preview',
    method: 'POST',
    data,
    ...options,
  })
}
