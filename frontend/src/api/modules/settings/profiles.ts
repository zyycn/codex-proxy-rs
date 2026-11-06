import request from '../../request'

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

export function isValidInstallationSeed(value: unknown) {
  return value == null || value === '' || (typeof value === 'string' && value.length === 36 && /^[\da-f]{8}(?:-[\da-f]{4}){3}-[\da-f]{12}$/i.test(value))
}

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
  })
}

interface ClientProfilePreviewParam {
  configuration: ClientProfileSelection | null
}

export function previewClientProfile(data: ClientProfilePreviewParam) {
  return request<ClientProfilePreview>({
    url: '/api/admin/settings/client-profiles/openai/preview',
    method: 'POST',
    data,
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

interface XaiClientProfileOptions {
  defaults: XaiClientProfileSelection
  globalConfiguration: XaiClientProfileSelection
}

interface XaiClientProfilePreviewParam {
  configuration: XaiClientProfileSelection | null
}

export function getXaiClientProfileOptions() {
  return request<XaiClientProfileOptions>({
    url: '/api/admin/settings/client-profiles/xai',
    method: 'GET',
  })
}

export function previewXaiClientProfile(data: XaiClientProfilePreviewParam) {
  return request<XaiClientProfilePreview>({
    url: '/api/admin/settings/client-profiles/xai/preview',
    method: 'POST',
    data,
    silent: true,
  })
}
