import type { RequestProfilePreview } from '@/api'

export interface ProfileSelection {
  versionMode: 'latest' | 'fixed'
  clientVersion: string | null
  clientIdentifier: string
  clientMode: string
  targetOs: string
  targetArch: string
}

export interface ProfilePreview extends RequestProfilePreview, Omit<ProfileSelection, 'versionMode'> {
  configuration: ProfileSelection
}

export interface ProfileOptions {
  defaults: ProfileSelection
  globalConfiguration: ProfileSelection
}
