import request from '../request'

export interface ClientProfileSelection {
  client: 'desktop' | 'cli'
  platform: 'macos' | 'linux' | 'windows'
  versionMode: 'latest' | 'fixed'
  originator: string | null
  osVersion: string | null
  arch: string | null
  terminal: string | null
  codexVersion: string | null
  desktopVersion: string | null
  desktopBuild: string | null
}

export interface ClientProfilePreview {
  configuration: ClientProfileSelection
  source: 'global' | 'override'
  originator: string
  osType: string
  osVersion: string
  arch: string
  terminal: string
  codexVersion: string
  desktopVersion: string | null
  desktopBuild: string | null
  userAgent: string
  versionSource: 'official' | 'custom'
  verifiedAt: string | null
  checkedAt: string | null
  error: string | null
}

export interface ClientProfilePreset {
  configuration: ClientProfileSelection
  automaticAvailable: boolean
  reason: string | null
  defaults: Pick<ClientProfileSelection, 'originator' | 'osVersion' | 'arch' | 'terminal'>
}

export function getClientProfileOptions() {
  return request<{ presets: ClientProfilePreset[], globalConfiguration: ClientProfileSelection }>({
    url: '/api/admin/settings/client-profiles/openai',
    method: 'GET',
    silent: true,
  })
}

export function previewClientProfile(configuration: ClientProfileSelection | null) {
  return request<ClientProfilePreview>({
    url: '/api/admin/settings/client-profiles/openai/preview',
    method: 'POST',
    data: { configuration },
    silent: true,
  })
}
