import type {
  FunctionalColorMap,
  PresetColorRoleMap,
  ResolvedThemeSeedTokens,
  ThemeAliasMap,
  ThemeColorRoleRecipe,
  ThemeDataMap,
  ThemeFillMap,
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
  compositeOnBackground,
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

const SURFACE_MAP_DERIVERS = {
  light: deriveNeutralLightSurfaceMap,
  dark: deriveNeutralDarkSurfaceMap,
} satisfies Record<ThemeName, (seedTokens: ResolvedThemeSeedTokens) => ThemeSurfaceMap>

const LIGHT_FILL_OPACITIES = {
  controlBg: 0.075,
  controlHoverBg: 0.105,
  controlActiveBg: 0.14,
  contentBg: 0.095,
  contentStripeBg: 0.045,
  contentHoverBg: 0.09,
} satisfies Record<keyof ThemeFillMap, number>

export function deriveLightThemeFillMap(seedTokens: ResolvedThemeSeedTokens): ThemeFillMap {
  // 先提亮色源并保留色相与饱和度，避免近黑色低透明度叠白后发灰。
  const fillColor = mixColorTone(WHITE, seedTokens.colorTextBase, 0.5)
  const fills = {} as ThemeFillMap
  for (const name of Object.keys(LIGHT_FILL_OPACITIES) as (keyof ThemeFillMap)[])
    fills[name] = withAlpha(fillColor, LIGHT_FILL_OPACITIES[name])
  return fills
}

function themeTextBackgrounds(surfaces: ThemeSurfaceMap, fills: ThemeFillMap | null, primary: ThemePrimaryMap): string[] {
  const hosts = [surfaces.colorBgLayout, surfaces.colorBgContainer, surfaces.colorBgElevated]
  const backgrounds = [
    ...hosts,
    surfaces.colorBgTextHover,
    surfaces.colorBgTextActive,
    surfaces.colorFillSecondary,
    surfaces.colorFillTertiary,
    surfaces.colorFillQuaternary,
  ]
  if (!fills)
    return backgrounds

  return [
    ...backgrounds,
    primary.colorPrimaryContainer,
    primary.colorPrimaryContainerHover,
    // 透明控件在页面、卡片和浮层上颜色不同，不能把带 Alpha 的值当作实色校正。
    ...hosts.flatMap(background => [fills.controlBg, fills.controlHoverBg, fills.controlActiveBg]
      .map(fill => compositeOnBackground(fill, background))),
    ...[fills.contentBg, fills.contentStripeBg, fills.contentHoverBg]
      .map(fill => compositeOnBackground(fill, surfaces.colorBgContainer)),
  ]
}

export function deriveThemeSurfaceMap(
  theme: ThemeName,
  seedTokens: ResolvedThemeSeedTokens,
): ThemeSurfaceMap {
  return SURFACE_MAP_DERIVERS[theme](seedTokens)
}

export function ensureThemeSurfaceContrast(
  surfaces: ThemeSurfaceMap,
  fills: ThemeFillMap | null,
  primary: ThemePrimaryMap,
): ThemeSurfaceMap {
  const backgrounds = themeTextBackgrounds(surfaces, fills, primary)

  // 辅助文字仍用于正常信息；按最深/最亮填充面保留可读的三级梯度。
  return {
    ...surfaces,
    colorText: ensureContrast(surfaces.colorText, backgrounds, 7),
    colorTextHeading: ensureContrast(surfaces.colorTextHeading, backgrounds, 7),
    colorTextSecondary: ensureContrast(surfaces.colorTextSecondary, backgrounds, 7),
    colorTextTertiary: ensureContrast(surfaces.colorTextTertiary, backgrounds, 5.5),
    colorTextQuaternary: ensureContrast(surfaces.colorTextQuaternary, backgrounds, 4.5),
  }
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
    colorPrimaryContainer: roles.container,
    colorPrimaryContainerHover: roles.containerHover,
    colorPrimaryBorder: roles.border,
    colorPrimaryBorderHover: roles.borderHover,
    colorPrimaryHover: roles.hover,
    colorPrimary: roles.color,
    colorPrimaryActive: roles.active,
    colorPrimaryTextHover: roles.textHover,
    colorPrimaryText: roles.text,
    colorPrimaryOnContainer: roles.onContainer,
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
  surfaces: ThemeSurfaceMap,
  seedTokens: ResolvedThemeSeedTokens,
  fills: ThemeFillMap | null,
  primary: ThemePrimaryMap,
): ThemeSemanticMap {
  const recipe = THEME_COLOR_ROLE_RECIPES[theme].semantic
  const containerBg = surfaces.colorBgContainer
  const textBackgrounds = themeTextBackgrounds(surfaces, fills, primary)

  return {
    info: deriveColorRoleMap(
      seedTokens.colorInfo,
      theme,
      containerBg,
      recipe,
      textBackgrounds,
    ),
    success: deriveColorRoleMap(
      seedTokens.colorSuccess,
      theme,
      containerBg,
      recipe,
      textBackgrounds,
    ),
    warning: deriveColorRoleMap(
      seedTokens.colorWarning,
      theme,
      containerBg,
      recipe,
      textBackgrounds,
    ),
    error: deriveColorRoleMap(
      seedTokens.colorError,
      theme,
      containerBg,
      recipe,
      textBackgrounds,
    ),
  }
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
  const activityBase = semantics.success.container
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

function deriveColorRoleMap(
  seed: string,
  theme: ThemeName,
  containerBg: string,
  recipe: ThemeColorRoleRecipe,
  textBackgrounds: string | readonly string[] = containerBg,
): FunctionalColorMap {
  const normalizedSeed = normalizeHexColor(seed) ?? DEFAULT_CUSTOM_THEME_COLOR
  const palette = generateColorPalette(normalizedSeed, theme, containerBg)
  const resolve = (source: ThemePaletteSource): string => source === 'seed'
    ? normalizedSeed
    : normalizeHexColor(palette[source - 1]) ?? normalizedSeed
  const color = resolve(recipe.color)
  const hover = resolve(recipe.hover)
  const active = resolve(recipe.active)
  const container = mix(containerBg, color, recipe.containerMix)
  const containerHover = mix(containerBg, hover, recipe.containerHoverMix)
  const containerActive = mix(containerBg, active, recipe.containerActiveMix)
  const semanticContainers = [container, containerHover, containerActive]
  const resolveText = (
    source: ThemePaletteSource,
    backgrounds: string | readonly string[] = textBackgrounds,
  ): string => {
    const value = resolve(source)
    const toned = recipe.minimumTextLightness === undefined
      ? value
      : ensureLightness(value, recipe.minimumTextLightness)
    return ensureContrast(toned, backgrounds, 4.5)
  }

  return {
    container,
    containerHover,
    containerActive,
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
    onContainer: resolveText(
      recipe.onContainer ?? recipe.text,
      semanticContainers,
    ),
    textActive: resolveText(recipe.textActive),
  }
}

/** 容器、边框和实心色对齐 Colorful Tag；文字在当前 Surface 上保持可读。 */
function derivePresetColorRoleMap(
  palette: readonly string[],
  containerBg: string,
  recipe: ThemePresetColorRoleRecipe,
): PresetColorRoleMap {
  const color = (step: number): string =>
    normalizeHexColor(palette[step - 1]) ?? DEFAULT_CUSTOM_THEME_COLOR
  const solid = color(recipe.solid)
  const container = mix(containerBg, solid, recipe.containerMix)
  const containerStrong = mix(containerBg, solid, recipe.containerStrongMix)
  const text = color(recipe.text)
  const onContainer = color(recipe.onContainer)
  const toneText = (value: string): string => recipe.minimumTextLightness === undefined
    ? value
    : ensureLightness(value, recipe.minimumTextLightness)

  return {
    container,
    containerStrong,
    border: ensureContrast(mix(containerBg, solid, recipe.borderMix), containerBg, 3),
    solid,
    text: ensureContrast(toneText(text), containerBg, 4.5),
    onContainer: ensureContrast(toneText(onContainer), [container, containerStrong], 4.5),
  }
}
