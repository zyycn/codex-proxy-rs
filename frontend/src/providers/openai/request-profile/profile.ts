import type { RequestProfilePreview } from '@/api'

export interface PresetSelection {
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

export interface CustomSelection {
  mode: 'custom'
  userAgent: string
  originator?: string | null
  codexVersion?: string | null
}

export type ProfileSelection = PresetSelection | CustomSelection

export interface ProfileOptions {
  presets: ProfilePreset[]
  globalConfiguration: ProfileSelection
  maxVersionLag: number
  recognizedUserAgentPrefixes: string[]
}

export interface ProfilePreview extends RequestProfilePreview {
  configuration: ProfileSelection
  originator: string
  osType: string
  osVersion: string
  arch: string
  terminal: string
  codexVersion: string
  desktopVersion: string | null
  desktopBuild: string | null
  versionLag: number | null
  recognized?: boolean
}

export interface ProfilePreset {
  configuration: PresetSelection
  automaticAvailable: boolean
  reason: string | null
  defaults: Pick<PresetSelection, 'originator' | 'osType' | 'osVersion' | 'arch' | 'terminal'>
}

export function needsCompanionHeaders(userAgent: string, options: ProfileOptions) {
  return !!userAgent.trim() && !options.recognizedUserAgentPrefixes.some(prefix => userAgent.startsWith(prefix))
}

export function needsProfileInput(selection: ProfileSelection, options: ProfileOptions) {
  if (selection.mode === 'custom') {
    return !selection.userAgent.trim()
      || (needsCompanionHeaders(selection.userAgent, options) && (!selection.originator?.trim() || !selection.codexVersion?.trim()))
  }
  return selection.versionMode === 'fixed' && (!selection.codexVersion
    || (selection.client === 'desktop' && (!selection.desktopVersion || !selection.desktopBuild)))
}
