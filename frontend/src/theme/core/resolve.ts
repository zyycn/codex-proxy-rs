import type {
  ResolvedTheme,
  ResolvedThemeSeedTokens,
  ThemeAppearanceSeed,
  ThemeColorId,
  ThemeColorPreset,
  ThemeColorPresetId,
  ThemeCustomization,
  ThemeMap,
  ThemeMode,
  ThemeName,
  ThemeSeedOverrides,
  ThemeTokens,
} from '../types'
import { normalizeHexColor } from '../../utils/color'
import { deriveCustomThemeAppearance } from '../derive/appearance'
import {
  deriveThemeAliasMap,
  deriveThemeDataMap,
  deriveThemeLinkMap,
  deriveThemePresetColorMap,
  deriveThemePrimaryMap,
  deriveThemeSemanticMap,
  deriveThemeSurfaceMap,
} from '../derive/colors'
import {
  deriveThemeComponentMap,
  deriveThemeDimensionMap,
  deriveThemeShadowMap,
} from '../derive/components'
import {
  DARK_CONTAINER_BASE,
  DARK_TEXT_BASE,
  DEFAULT_CUSTOM_THEME_COLOR,
  DEFAULT_DIMENSIONS,
  DEFAULT_SEED_COLORS,
  DEFAULT_THEME_CUSTOMIZATION,
  LIGHT_CONTAINER_BASE,
  LIGHT_TEXT_BASE,
  THEME_COLOR_PRESETS,
} from './constants'
import { normalizeThemeCustomization } from './normalize'
import { createThemeTokens } from './tokens'

export function resolveThemeName(mode: ThemeMode, prefersDark: boolean): ThemeName {
  if (mode === 'system')
    return prefersDark ? 'dark' : 'light'
  return mode
}

export function resolveThemeSeed(colorId: ThemeColorId, customColor: string): string {
  if (colorId === 'custom')
    return normalizeHexColor(customColor) ?? DEFAULT_CUSTOM_THEME_COLOR
  return themeColorPreset(colorId).seed
}

export function resolveTheme(
  name: ThemeName,
  colorId: ThemeColorId,
  customColor: string,
  customization: ThemeCustomization = DEFAULT_THEME_CUSTOMIZATION,
): ResolvedTheme {
  const seed = resolveThemeSeed(colorId, customColor)
  const normalizedCustomization = normalizeThemeCustomization(customization)
  const appearance = colorId === 'custom'
    ? deriveCustomThemeAppearance(name, seed)
    : themeColorPreset(colorId).appearance?.[name]
  const seedTokens = resolveThemeSeedTokens(
    name,
    seed,
    normalizedCustomization.seed,
    appearance,
  )
  const themeMap = deriveThemeMap(name, seed, seedTokens, normalizedCustomization)
  const tokensWithOverrides: ThemeTokens = {
    ...createThemeTokens(themeMap),
    ...normalizedCustomization.tokenOverrides,
  }

  return {
    name,
    colorId,
    seed,
    seedTokens,
    tokens: tokensWithOverrides,
  }
}

export function themeColorPreset(id: ThemeColorPresetId): ThemeColorPreset {
  return THEME_COLOR_PRESETS.find(preset => preset.id === id) ?? THEME_COLOR_PRESETS[0]!
}

function deriveThemeMap(
  name: ThemeName,
  seed: string,
  seedTokens: ResolvedThemeSeedTokens,
  customization: ThemeCustomization,
): ThemeMap {
  const surfaces = deriveThemeSurfaceMap(name, seedTokens)
  const aliases = deriveThemeAliasMap(surfaces)
  const primary = deriveThemePrimaryMap(seed, name, surfaces.colorBgContainer)
  const link = deriveThemeLinkMap(seedTokens.colorLink, name, surfaces.colorBgContainer)
  const semantics = deriveThemeSemanticMap(name, surfaces.colorBgContainer, seedTokens)

  return {
    surfaces,
    aliases,
    primary,
    link,
    semantics,
    presetColors: deriveThemePresetColorMap(name, surfaces.colorBgContainer, seedTokens),
    data: deriveThemeDataMap(aliases, semantics),
    shadows: deriveThemeShadowMap(name, seedTokens.shadowStrength),
    components: deriveThemeComponentMap(
      name,
      surfaces,
      aliases,
      primary,
      semantics.error,
      seedTokens.shadowStrength,
    ),
    dimensions: deriveThemeDimensionMap(seedTokens, customization.component),
  }
}

function resolveThemeSeedTokens(
  theme: ThemeName,
  colorPrimary: string,
  overrides: ThemeSeedOverrides | undefined,
  appearance: ThemeAppearanceSeed | undefined,
): ResolvedThemeSeedTokens {
  return {
    colorPrimary,
    colorSuccess: overrides?.colorSuccess ?? DEFAULT_SEED_COLORS.colorSuccess,
    colorWarning: overrides?.colorWarning ?? DEFAULT_SEED_COLORS.colorWarning,
    colorError: overrides?.colorError ?? DEFAULT_SEED_COLORS.colorError,
    colorInfo: overrides?.colorInfo ?? DEFAULT_SEED_COLORS.colorInfo,
    colorLink: overrides?.colorLink ?? colorPrimary,
    colorTextBase: overrides?.colorTextBase
      ?? appearance?.colorTextBase
      ?? (theme === 'dark' ? DARK_TEXT_BASE : LIGHT_TEXT_BASE),
    colorBgBase: overrides?.colorBgBase
      ?? appearance?.colorBgBase
      ?? (theme === 'dark' ? DARK_CONTAINER_BASE : LIGHT_CONTAINER_BASE),
    fontSize: overrides?.fontSize ?? DEFAULT_DIMENSIONS.fontSize,
    sizeUnit: overrides?.sizeUnit ?? DEFAULT_DIMENSIONS.sizeUnit,
    sizeStep: overrides?.sizeStep ?? DEFAULT_DIMENSIONS.sizeStep,
    controlHeight: overrides?.controlHeight ?? DEFAULT_DIMENSIONS.controlHeight,
    borderRadius: overrides?.borderRadius ?? DEFAULT_DIMENSIONS.borderRadius,
    shadowStrength: overrides?.shadowStrength ?? DEFAULT_DIMENSIONS.shadowStrength,
  }
}
