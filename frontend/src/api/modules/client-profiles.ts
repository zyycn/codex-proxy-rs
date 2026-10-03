import request from '../request'

export type ProviderRequestProfile = Record<string, unknown>
export type ProviderRequestProfiles = Record<string, ProviderRequestProfile>
export type ProviderRequestProfileUpdates = Record<string, ProviderRequestProfile | null>

export interface PresetClientProfileSelection {
  mode?: undefined
  client: 'desktop' | 'cli'
  platform: 'macos' | 'linux' | 'windows'
  versionMode: 'latest' | 'fixed'
  versionLag?: number | null
  cliEntry?: 'tui' | 'exec' | null
  osType?: string | null
  originator: string | null
  osVersion: string | null
  arch: string | null
  terminal: string | null
  codexVersion: string | null
  desktopVersion: string | null
  desktopBuild: string | null
}

export interface CustomClientProfileSelection {
  mode: 'custom'
  userAgent: string
  originator?: string | null
  codexVersion?: string | null
}

export type ClientProfileSelection = PresetClientProfileSelection | CustomClientProfileSelection

export interface ClientProfileOptions {
  presets: ClientProfilePreset[]
  globalConfiguration: ClientProfileSelection
  maxVersionLag: number
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
  versionLag: number | null
  recognized?: boolean
  verifiedAt: string | null
  verifiedAtDisplay: string | null
  checkedAt: string | null
  checkedAtDisplay: string | null
  error: string | null
}

export interface ClientProfilePreset {
  configuration: PresetClientProfileSelection
  automaticAvailable: boolean
  reason: string | null
  defaults: Pick<PresetClientProfileSelection, 'originator' | 'osType' | 'osVersion' | 'arch' | 'terminal'>
}

export function getClientProfileOptions() {
  return request<ClientProfileOptions>({
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

export interface XaiClientProfileSelection {
  versionMode: 'latest' | 'fixed'
  clientVersion: string | null
  clientIdentifier: string
  clientMode: string
  targetOs: string
  targetArch: string
}

export interface XaiClientProfilePreview extends Omit<XaiClientProfileSelection, 'versionMode'> {
  configuration: XaiClientProfileSelection
  source: 'global' | 'override'
  userAgent: string
  versionSource: 'official' | 'custom'
  verifiedAt: string | null
  verifiedAtDisplay: string | null
  checkedAt: string | null
  checkedAtDisplay: string | null
  error: string | null
}

export function getXaiClientProfileOptions() {
  return request<{ defaults: XaiClientProfileSelection, globalConfiguration: XaiClientProfileSelection }>({
    url: '/api/admin/settings/client-profiles/xai',
    method: 'GET',
    silent: true,
  })
}

export function previewXaiClientProfile(configuration: XaiClientProfileSelection | null) {
  return request<XaiClientProfilePreview>({
    url: '/api/admin/settings/client-profiles/xai/preview',
    method: 'POST',
    data: { configuration },
    silent: true,
  })
}
