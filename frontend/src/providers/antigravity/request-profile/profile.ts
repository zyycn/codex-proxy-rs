import type { RequestProfilePreview } from '@/api'

export interface ProfileSelection {
  client: 'desktop' | 'cli'
  platform: 'macos' | 'linux' | 'windows'
  versionMode: 'latest' | 'fixed'
  clientVersion: string | null
  arch: string | null
  systemPrompt?: string | null
}

export interface ProfileOptions {
  defaultSystemPrompt: string
  systemPromptSource: string
  defaults: ProfileSelection
  presets: {
    configuration: ProfileSelection
    automaticAvailable: boolean
    reason: string | null
  }[]
  globalConfiguration: ProfileSelection
}

export interface ProfilePreview extends RequestProfilePreview {
  configuration: ProfileSelection
  product: string
  osType: string
  arch: string
  clientVersion: string
}
