import type { ThemeColorPresetId, ThemeOptions, ThemeSeedOverrides } from '@codex-proxy/ui/theme'
import { createTheme, createThemeSeedTokens, normalizeThemeCustomization, themeProfiles } from '@codex-proxy/ui/theme'
import { omit, pick } from 'es-toolkit'

const independentColors = [
  'colorSuccess',
  'colorWarning',
  'colorError',
  'colorInfo',
  'colorLink',
  'colorTextBase',
  'colorBgBase',
] as const

export function createAdminTheme(options: ThemeOptions) {
  const defaults = createThemeSeedTokens({ scheme: options.scheme })
  return createTheme({
    ...options,
    customization: {
      ...options.customization,
      seed: {
        ...pick(defaults, independentColors),
        ...options.customization?.seed,
      },
    },
  })
}

export function profileCustomization(color: ThemeColorPresetId, currentSeed: ThemeSeedOverrides = {}) {
  const { customization } = themeProfiles[color]
  return normalizeThemeCustomization({
    ...customization,
    seed: {
      ...omit(customization.seed ?? {}, independentColors),
      ...pick(currentSeed, independentColors),
    },
  })
}
