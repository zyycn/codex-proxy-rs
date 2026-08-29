import type {
  FunctionalColorMap,
  PresetColorRoleMap,
  ResolvedThemeSeedTokens,
  ThemeAliasMap,
  ThemeColorRoleRecipe,
  ThemeDataMap,
  ThemeLinkMap,
  ThemeName,
  ThemePaletteSource,
  ThemePresetColorMap,
  ThemePresetColorName,
  ThemePresetColorRoleRecipe,
  ThemePrimaryMap,
  ThemeSemanticMap,
  ThemeSurfaceMap,
} from '../types'
import { normalizeHexColor } from '../../utils/color'
import {
  ensureContrast,
  ensureLightness,
  generateColorPalette,
  mix,
  mixColorTone,
  relativeColorDistance,
  withAlpha,
} from '../core/color'
import {
  BLACK,
  DARK_CONTAINER_BASE,
  DARK_SEMANTIC_TEXT_ANCHORS,
  DARK_SURFACE_ANCHORS,
  DARK_TEXT_BASE,
  DEFAULT_CUSTOM_THEME_COLOR,
  LIGHT_CONTAINER_BASE,
  LIGHT_FOREGROUND,
  LIGHT_SURFACE_ANCHORS,
  LIGHT_TEXT_BASE,
  PRESET_COLOR_SEEDS,
  WHITE,
} from '../core/constants'
import { THEME_COLOR_ROLE_RECIPES } from './roles'

const FULL_SURFACE_APPEARANCE_DISTANCE = 0.02
const ACTIVITY_LEVEL_MIX = {
  low: 0.22,
  medium: 0.46,
  high: 0.7,
} as const

const SEMANTIC_MAP_DERIVERS = {
  light: (_containerBg: string, semantics: ThemeSemanticMap): ThemeSemanticMap => semantics,
  dark: deriveDarkSemanticMap,
} satisfies Record<
  ThemeName,
  (containerBg: string, semantics: ThemeSemanticMap) => ThemeSemanticMap
>

const SURFACE_MAP_DERIVERS = {
  light: deriveNeutralLightSurfaceMap,
  dark: deriveNeutralDarkSurfaceMap,
} satisfies Record<ThemeName, (seedTokens: ResolvedThemeSeedTokens) => ThemeSurfaceMap>

export function deriveThemeSurfaceMap(
  theme: ThemeName,
  seedTokens: ResolvedThemeSeedTokens,
): ThemeSurfaceMap {
  return SURFACE_MAP_DERIVERS[theme](seedTokens)
}

export function deriveThemeAliasMap(surfaces: ThemeSurfaceMap): ThemeAliasMap {
  return {
    colorFillAlter: surfaces.colorFillQuaternary,
  }
}

export function deriveThemePrimaryMap(
  seed: string,
  theme: ThemeName,
  containerBg: string,
): ThemePrimaryMap {
  const roles = deriveColorRoleMap(
    seed,
    theme,
    containerBg,
    THEME_COLOR_ROLE_RECIPES[theme].primary,
  )

  return {
    colorPrimaryBg: roles.background,
    colorPrimaryBgHover: roles.backgroundHover,
    colorPrimaryBorder: roles.border,
    colorPrimaryBorderHover: roles.borderHover,
    colorPrimaryHover: roles.hover,
    colorPrimary: roles.color,
    colorPrimaryActive: roles.active,
    colorPrimaryTextHover: roles.textHover,
    colorPrimaryText: roles.text,
    colorPrimaryTextActive: roles.textActive,
    colorTextLightSolid: LIGHT_FOREGROUND,
  }
}

export function deriveThemeLinkMap(
  seed: string,
  theme: ThemeName,
  containerBg: string,
): ThemeLinkMap {
  const roles = deriveColorRoleMap(
    seed,
    theme,
    containerBg,
    THEME_COLOR_ROLE_RECIPES[theme].primary,
  )

  return {
    colorLinkHover: roles.textHover,
    colorLink: roles.text,
    colorLinkActive: roles.textActive,
  }
}

export function deriveThemeSemanticMap(
  theme: ThemeName,
  containerBg: string,
  seedTokens: ResolvedThemeSeedTokens,
): ThemeSemanticMap {
  const recipe = THEME_COLOR_ROLE_RECIPES[theme].semantic

  const semantics = {
    info: deriveColorRoleMap(seedTokens.colorInfo, theme, containerBg, recipe),
    success: deriveColorRoleMap(seedTokens.colorSuccess, theme, containerBg, recipe),
    warning: deriveColorRoleMap(seedTokens.colorWarning, theme, containerBg, recipe),
    error: deriveColorRoleMap(seedTokens.colorError, theme, containerBg, recipe),
  }

  return SEMANTIC_MAP_DERIVERS[theme](containerBg, semantics)
}

export function deriveThemePresetColorMap(
  theme: ThemeName,
  containerBg: string,
  seedTokens: ResolvedThemeSeedTokens,
): ThemePresetColorMap {
  const seeds: Record<ThemePresetColorName, string> = {
    ...PRESET_COLOR_SEEDS,
    blue: seedTokens.colorInfo,
    green: seedTokens.colorSuccess,
    orange: seedTokens.colorWarning,
    red: seedTokens.colorError,
  }
  const presetColors = {} as ThemePresetColorMap

  for (const colorName of Object.keys(seeds) as ThemePresetColorName[]) {
    presetColors[colorName] = derivePresetColorRoleMap(
      generateColorPalette(seeds[colorName], theme, containerBg),
      containerBg,
      THEME_COLOR_ROLE_RECIPES[theme].preset,
    )
  }

  return presetColors
}

export function deriveThemeDataMap(
  aliases: ThemeAliasMap,
  semantics: ThemeSemanticMap,
): ThemeDataMap {
  const activityBase = semantics.success.background
  const activitySolid = semantics.success.color

  return {
    activityLevels: [
      aliases.colorFillAlter,
      mix(activityBase, activitySolid, ACTIVITY_LEVEL_MIX.low),
      mix(activityBase, activitySolid, ACTIVITY_LEVEL_MIX.medium),
      mix(activityBase, activitySolid, ACTIVITY_LEVEL_MIX.high),
      activitySolid,
    ],
  }
}

function deriveNeutralDarkSurfaceMap(
  seedTokens: ResolvedThemeSeedTokens,
): ThemeSurfaceMap {
  const background = seedTokens.colorBgBase
  const colorText = seedTokens.colorTextBase
  const appearanceInfluence = deriveSurfaceAppearanceInfluence(
    background,
    colorText,
    DARK_CONTAINER_BASE,
    DARK_TEXT_BASE,
  )
  const adaptiveTone = (neutral: string, chromatic: string): string =>
    mix(neutral, chromatic, appearanceInfluence)
  const neutralFillQuaternary = mixColorTone(background, colorText, 0.04, {
    hueWeight: 0,
  })
  const neutralFillTertiary = mixColorTone(background, colorText, 0.0765, {
    hueWeight: 0.64,
    saturationScale: 0.95,
  })
  const neutralFillSecondary = mixColorTone(background, colorText, 0.139, {
    hueWeight: 0.52,
    saturationScale: 0.99,
  })
  const neutralBorderSecondary = mixColorTone(background, colorText, 0.1395, {
    hueWeight: 0.62,
    saturationScale: 0.85,
  })
  const neutralBorder = mixColorTone(background, colorText, 0.1495, {
    hueWeight: 0.58,
    saturationScale: 0.78,
  })
  const colorFillQuaternary = adaptiveTone(
    neutralFillQuaternary,
    mix(background, colorText, 0.045),
  )
  const colorFillTertiary = adaptiveTone(
    neutralFillTertiary,
    mix(background, colorText, 0.075),
  )
  const colorFillSecondary = adaptiveTone(
    neutralFillSecondary,
    mix(background, colorText, 0.12),
  )
  const colorBorderSecondary = adaptiveTone(
    neutralBorderSecondary,
    mix(background, colorText, 0.105),
  )
  const colorBorder = adaptiveTone(
    neutralBorder,
    mix(background, colorText, 0.155),
  )
  const adaptiveMutedText = (
    weight: number,
    hueWeight: number,
    saturationScale: number,
    chromaticBackgroundWeight: number,
  ): string => adaptiveTone(
    mixColorTone(
      background,
      colorText,
      weight,
      { hueWeight, saturationScale },
    ),
    mix(colorText, background, chromaticBackgroundWeight),
  )

  return {
    colorBgLayout: adaptiveTone(
      DARK_SURFACE_ANCHORS.colorBgLayout,
      mix(background, BLACK, 0.3),
    ),
    colorBgContainer: background,
    colorBgElevated: adaptiveTone(
      neutralFillQuaternary,
      mix(background, WHITE, 0.045),
    ),
    colorBgSpotlight: adaptiveTone(
      DARK_SURFACE_ANCHORS.colorBgSpotlight,
      mix(background, BLACK, 0.74),
    ),
    colorBgMask: withAlpha(
      adaptiveTone(
        DARK_SURFACE_ANCHORS.colorBgMask,
        mix(background, BLACK, 0.8),
      ),
      0.65 + appearanceInfluence * 0.03,
    ),
    colorBgTextHover: adaptiveTone(
      mixColorTone(background, colorText, 0.0815, {
        hueWeight: 0.5,
        saturationScale: 1.04,
      }),
      mix(background, colorText, 0.08),
    ),
    colorBgTextActive: colorFillSecondary,
    colorBgContainerDisabled: adaptiveTone(
      mixColorTone(background, colorText, 0.036, {
        hueWeight: 0.5,
        saturationScale: 0.88,
      }),
      mix(background, colorText, 0.035),
    ),
    colorFillSecondary,
    colorFillTertiary,
    colorFillQuaternary,
    colorBorder,
    colorBorderSecondary,
    colorSplit: withAlpha(colorBorderSecondary, 0.5),
    colorText,
    colorTextHeading: adaptiveTone(
      mixColorTone(WHITE, colorText, 0.23, {
        saturationScale: 1.35,
      }),
      mix(colorText, WHITE, 0.74),
    ),
    colorTextSecondary: adaptiveMutedText(0.6745, 0.7, 0.55, 0.36),
    colorTextTertiary: adaptiveMutedText(0.531, 0.58, 0.45, 0.5),
    colorTextQuaternary: adaptiveMutedText(0.4555, 0.48, 0.42, 0.58),
    colorTextDisabled: adaptiveMutedText(0.37, 1, 0.48, 0.66),
    colorShadow: withAlpha(BLACK, 0.75),
  }
}

function deriveNeutralLightSurfaceMap(
  seedTokens: ResolvedThemeSeedTokens,
): ThemeSurfaceMap {
  const background = seedTokens.colorBgBase
  const colorText = seedTokens.colorTextBase
  const appearanceInfluence = deriveSurfaceAppearanceInfluence(
    background,
    colorText,
    LIGHT_CONTAINER_BASE,
    LIGHT_TEXT_BASE,
  )
  const adaptiveTone = (neutral: string, chromatic: string): string =>
    mix(neutral, chromatic, appearanceInfluence)
  const colorFillQuaternary = adaptiveTone(
    LIGHT_SURFACE_ANCHORS.colorFillQuaternary,
    mix(background, colorText, 0.035),
  )
  const colorFillTertiary = adaptiveTone(
    LIGHT_SURFACE_ANCHORS.colorFillTertiary,
    mix(background, colorText, 0.06),
  )
  const colorFillSecondary = adaptiveTone(
    LIGHT_SURFACE_ANCHORS.colorFillSecondary,
    mix(background, colorText, 0.09),
  )
  const colorBorderSecondary = adaptiveTone(
    LIGHT_SURFACE_ANCHORS.colorBorderSecondary,
    mix(background, colorText, 0.105),
  )
  const colorBorder = adaptiveTone(
    LIGHT_SURFACE_ANCHORS.colorBorder,
    mix(background, colorText, 0.145),
  )

  return {
    colorBgLayout: colorFillQuaternary,
    colorBgContainer: background,
    colorBgElevated: background,
    colorBgSpotlight: adaptiveTone(
      LIGHT_SURFACE_ANCHORS.colorBgSpotlight,
      mix(colorText, BLACK, 0.08),
    ),
    colorBgMask: withAlpha(colorText, 0.3),
    colorBgTextHover: colorFillTertiary,
    colorBgTextActive: colorFillSecondary,
    colorBgContainerDisabled: colorFillTertiary,
    colorFillSecondary,
    colorFillTertiary,
    colorFillQuaternary,
    colorBorder,
    colorBorderSecondary,
    colorSplit: withAlpha(colorBorder, 0.42),
    colorText,
    colorTextHeading: adaptiveTone(
      LIGHT_SURFACE_ANCHORS.colorTextHeading,
      mix(colorText, BLACK, 0.18),
    ),
    colorTextSecondary: adaptiveTone(
      LIGHT_SURFACE_ANCHORS.colorTextSecondary,
      mix(colorText, background, 0.36),
    ),
    colorTextTertiary: adaptiveTone(
      LIGHT_SURFACE_ANCHORS.colorTextMuted,
      mix(colorText, background, 0.55),
    ),
    colorTextQuaternary: adaptiveTone(
      LIGHT_SURFACE_ANCHORS.colorTextMuted,
      mix(colorText, background, 0.6),
    ),
    colorTextDisabled: adaptiveTone(
      LIGHT_SURFACE_ANCHORS.colorTextMuted,
      mix(colorText, background, 0.62),
    ),
    colorShadow: withAlpha(colorText, 0.38),
  }
}

function deriveSurfaceAppearanceInfluence(
  background: string,
  text: string,
  baselineBackground: string,
  baselineText: string,
): number {
  const distance = Math.max(
    relativeColorDistance(background, baselineBackground),
    relativeColorDistance(text, baselineText),
  )
  const normalized = Math.min(1, distance / FULL_SURFACE_APPEARANCE_DISTANCE)
  return normalized * normalized * (3 - 2 * normalized)
}

function deriveDarkSemanticMap(
  containerBg: string,
  semantics: ThemeSemanticMap,
): ThemeSemanticMap {
  const appearanceInfluence = deriveSingleColorAppearanceInfluence(
    containerBg,
    DARK_CONTAINER_BASE,
  )

  return {
    info: withSemanticTextAnchor(
      semantics.info,
      DARK_SEMANTIC_TEXT_ANCHORS.info,
      appearanceInfluence,
    ),
    success: withSemanticTextAnchor(
      semantics.success,
      DARK_SEMANTIC_TEXT_ANCHORS.success,
      appearanceInfluence,
    ),
    warning: withSemanticTextAnchor(
      semantics.warning,
      DARK_SEMANTIC_TEXT_ANCHORS.warning,
      appearanceInfluence,
    ),
    error: withSemanticTextAnchor(
      semantics.error,
      DARK_SEMANTIC_TEXT_ANCHORS.error,
      appearanceInfluence,
    ),
  }
}

function withSemanticTextAnchor(
  colorMap: FunctionalColorMap,
  anchor: string,
  appearanceInfluence: number,
): FunctionalColorMap {
  return {
    ...colorMap,
    text: mix(anchor, colorMap.text, appearanceInfluence),
  }
}

function deriveSingleColorAppearanceInfluence(
  color: string,
  baseline: string,
): number {
  const distance = relativeColorDistance(color, baseline)
  const normalized = Math.min(1, distance / FULL_SURFACE_APPEARANCE_DISTANCE)
  return normalized * normalized * (3 - 2 * normalized)
}

function deriveColorRoleMap(
  seed: string,
  theme: ThemeName,
  containerBg: string,
  recipe: ThemeColorRoleRecipe,
): FunctionalColorMap {
  const normalizedSeed = normalizeHexColor(seed) ?? DEFAULT_CUSTOM_THEME_COLOR
  const palette = generateColorPalette(normalizedSeed, theme, containerBg)
  const resolve = (source: ThemePaletteSource): string => source === 'seed'
    ? normalizedSeed
    : normalizeHexColor(palette[source - 1]) ?? normalizedSeed
  const color = resolve(recipe.color)
  const hover = resolve(recipe.hover)
  const active = resolve(recipe.active)
  const resolveText = (source: ThemePaletteSource): string => {
    const value = resolve(source)
    const toned = recipe.minimumTextLightness === undefined
      ? value
      : ensureLightness(value, recipe.minimumTextLightness)
    return ensureContrast(toned, containerBg, 4.5)
  }

  return {
    background: mix(containerBg, color, recipe.backgroundMix),
    backgroundHover: mix(containerBg, hover, recipe.backgroundHoverMix),
    backgroundActive: mix(containerBg, active, recipe.backgroundActiveMix),
    border: ensureContrast(mix(containerBg, color, recipe.borderMix), containerBg, 3),
    borderHover: ensureContrast(mix(containerBg, hover, recipe.borderHoverMix), containerBg, 3),
    hover: recipe.hoverContrast === undefined
      ? hover
      : ensureContrast(hover, containerBg, recipe.hoverContrast),
    color,
    active: recipe.activeContrast === undefined
      ? active
      : ensureContrast(active, containerBg, recipe.activeContrast),
    textHover: resolveText(recipe.textHover),
    text: resolveText(recipe.text),
    textActive: resolveText(recipe.textActive),
  }
}

/** 背景、边框和实心色对齐 Colorful Tag；文字在当前 Surface 上保持可读。 */
function derivePresetColorRoleMap(
  palette: readonly string[],
  containerBg: string,
  recipe: ThemePresetColorRoleRecipe,
): PresetColorRoleMap {
  const color = (step: number): string =>
    normalizeHexColor(palette[step - 1]) ?? DEFAULT_CUSTOM_THEME_COLOR
  const solid = color(recipe.solid)
  const background = mix(containerBg, solid, recipe.backgroundMix)
  const backgroundStrong = mix(containerBg, solid, recipe.backgroundStrongMix)
  const text = color(recipe.text)
  const textOnBackground = color(recipe.textOnBackground)
  const toneText = (value: string): string => recipe.minimumTextLightness === undefined
    ? value
    : ensureLightness(value, recipe.minimumTextLightness)

  return {
    background,
    backgroundStrong,
    border: ensureContrast(mix(containerBg, solid, recipe.borderMix), containerBg, 3),
    solid,
    text: ensureContrast(toneText(text), containerBg, 4.5),
    textOnBackground: ensureContrast(toneText(textOnBackground), backgroundStrong, 4.5),
  }
}
