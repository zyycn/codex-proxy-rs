import type {
  FunctionalColorMap,
  ResolvedThemeSeedTokens,
  ThemeAliasMap,
  ThemeComponentMap,
  ThemeComponentOverrides,
  ThemeDimensionMap,
  ThemeName,
  ThemePrimaryMap,
  ThemeShadowMap,
  ThemeSurfaceMap,
} from '../types'
import {
  mix,
  relativeColorDistance,
  scaleShadowAlpha,
  withAlpha,
} from '../core/color'
import {
  BLACK,
  DARK_COMPONENT_ANCHORS,
  DARK_CONTAINER_BASE,
  DARK_TEXT_BASE,
  DEFAULT_DIMENSIONS,
  LIGHT_COMPONENT_ANCHORS,
  LIGHT_CONTAINER_BASE,
  LIGHT_SHADOW_BASE,
  LIGHT_TEXT_BASE,
  WHITE,
} from '../core/constants'

const FULL_COMPONENT_APPEARANCE_DISTANCE = 0.02

type ComponentMapDeriver = (
  surfaces: ThemeSurfaceMap,
  aliases: ThemeAliasMap,
  primary: ThemePrimaryMap,
  error: FunctionalColorMap,
  shadowStrength: number,
) => ThemeComponentMap

const SHADOW_MAP_DERIVERS = {
  light: deriveLightShadowMap,
  dark: deriveDarkShadowMap,
} satisfies Record<ThemeName, (shadowStrength: number) => ThemeShadowMap>

const COMPONENT_MAP_DERIVERS = {
  light: deriveLightComponentMap,
  dark: deriveDarkComponentMap,
} satisfies Record<ThemeName, ComponentMapDeriver>

export function deriveThemeDimensionMap(
  seedTokens: ResolvedThemeSeedTokens,
  componentOverrides: ThemeComponentOverrides | undefined,
): ThemeDimensionMap {
  const fontSize = seedTokens.fontSize
  const fontSizeXs = Math.max(9, fontSize - 2)
  const fontSizeSm = Math.max(10, fontSize - 1)
  const fontSizeLg = fontSize + 1
  const fontSizeXl = fontSize + 2
  const controlHeight = seedTokens.controlHeight
  const controlHeightStep = Math.max(2, Math.round(seedTokens.sizeStep * 1.5))
  const tableRowHeight = componentOverrides?.tableRowHeight ?? DEFAULT_DIMENSIONS.tableRowHeight
  const borderRadius = seedTokens.borderRadius
  const radiusStep = Math.max(1, Math.round(seedTokens.sizeStep / 2))
  const cardBorderRadius = componentOverrides?.cardBorderRadius
    ?? DEFAULT_DIMENSIONS.cardBorderRadius

  return {
    fontSizeXs: `${fontSizeXs}px`,
    fontSizeSm: `${fontSizeSm}px`,
    fontSize: `${fontSize}px`,
    fontSizeLg: `${fontSizeLg}px`,
    fontSizeXl: `${fontSizeXl}px`,
    lineHeightXs: `${fontSizeXs + 5}px`,
    lineHeightSm: `${fontSizeSm + 6}px`,
    lineHeight: `${fontSize + 7}px`,
    lineHeightLg: `${fontSizeLg + 8}px`,
    lineHeightXl: `${fontSizeXl + 9}px`,
    sizeUnit: `${seedTokens.sizeUnit}px`,
    sizeStep: `${seedTokens.sizeStep}px`,
    controlHeightSm: `${Math.max(24, controlHeight - controlHeightStep)}px`,
    controlHeight: `${controlHeight}px`,
    controlHeightLg: `${controlHeight + controlHeightStep}px`,
    tableRowHeight: `${tableRowHeight}px`,
    tableRowHeightSm: `${Math.max(32, tableRowHeight - 20)}px`,
    borderRadiusSm: `${Math.max(0, borderRadius - radiusStep)}px`,
    borderRadius: `${borderRadius}px`,
    borderRadiusLg: `${borderRadius + seedTokens.sizeStep}px`,
    cardBorderRadius: `${cardBorderRadius}px`,
  }
}

export function deriveThemeShadowMap(
  theme: ThemeName,
  shadowStrength: number,
): ThemeShadowMap {
  return SHADOW_MAP_DERIVERS[theme](shadowStrength)
}

function deriveDarkShadowMap(shadowStrength: number): ThemeShadowMap {
  return {
    boxShadow: scaleShadowAlpha(`0 24px 52px -26px ${withAlpha(BLACK, 0.82)}`, shadowStrength),
    boxShadowSecondary: scaleShadowAlpha(`0 18px 34px -26px ${withAlpha(BLACK, 0.64)}`, shadowStrength),
    boxShadowTertiary: scaleShadowAlpha(`0 12px 24px -20px ${withAlpha(BLACK, 0.66)}`, shadowStrength),
  }
}

function deriveLightShadowMap(shadowStrength: number): ThemeShadowMap {
  return {
    boxShadow: scaleShadowAlpha(`0 18px 38px -18px ${withAlpha(LIGHT_SHADOW_BASE, 0.17)}`, shadowStrength),
    boxShadowSecondary: scaleShadowAlpha(`0 10px 22px -18px ${withAlpha(LIGHT_SHADOW_BASE, 0.08)}`, shadowStrength),
    boxShadowTertiary: scaleShadowAlpha(`0 9px 18px -14px ${withAlpha(LIGHT_SHADOW_BASE, 0.086)}`, shadowStrength),
  }
}

export function deriveThemeComponentMap(
  theme: ThemeName,
  surfaces: ThemeSurfaceMap,
  aliases: ThemeAliasMap,
  primary: ThemePrimaryMap,
  error: FunctionalColorMap,
  shadowStrength: number,
): ThemeComponentMap {
  return COMPONENT_MAP_DERIVERS[theme](
    surfaces,
    aliases,
    primary,
    error,
    shadowStrength,
  )
}

function deriveDarkComponentMap(
  surfaces: ThemeSurfaceMap,
  aliases: ThemeAliasMap,
  primary: ThemePrimaryMap,
  error: FunctionalColorMap,
  shadowStrength: number,
): ThemeComponentMap {
  const inputOutlineColor = 'var(--cp-color-primary-bg-hover)'
  const appearanceInfluence = deriveComponentAppearanceInfluence(
    surfaces.colorBgContainer,
    DARK_CONTAINER_BASE,
    surfaces.colorText,
    DARK_TEXT_BASE,
  )

  return {
    menuItemSelectedBg: surfaces.colorBgTextHover,
    inputBg: mix(surfaces.colorBgContainer, surfaces.colorFillSecondary, 0.375),
    inputHoverBg: mix(surfaces.colorBgContainer, surfaces.colorFillSecondary, 0.69),
    inputActiveBg: mix(
      DARK_COMPONENT_ANCHORS.inputActiveBg,
      mix(surfaces.colorBgContainer, surfaces.colorBgElevated, 0.4),
      appearanceInfluence,
    ),
    inputErrorActiveBg: error.background,
    buttonPrimaryColor: primary.colorTextLightSolid,
    buttonPrimaryBg: primary.colorPrimary,
    buttonPrimaryHoverBg: mix(primary.colorPrimary, WHITE, 0.06),
    buttonPrimaryActiveBg: mix(primary.colorPrimary, BLACK, 0.08),
    brandMarkBg: surfaces.colorBgElevated,
    cardBg: surfaces.colorBgContainer,
    modalBg: surfaces.colorBgContainer,
    tableHeaderBg: surfaces.colorFillTertiary,
    tableRowBg: surfaces.colorBgContainer,
    tableRowStripeBg: aliases.colorFillAlter,
    tableRowHoverBg: surfaces.colorBgTextHover,
    tableRowSelectedBg: primary.colorPrimaryBg,
    progressRemainingColor: mix(surfaces.colorBgContainer, surfaces.colorBorderSecondary, 0.92),
    layoutSiderBg: surfaces.colorBgContainer,
    cardShadow: scaleShadowAlpha(`0 18px 34px -26px ${withAlpha(BLACK, 0.64)}`, shadowStrength),
    inputShadow: scaleShadowAlpha(`0 12px 24px -20px ${withAlpha(BLACK, 0.66)}`, shadowStrength),
    inputHoverShadow: scaleShadowAlpha(
      `0 0 0 3px ${inputOutlineColor}, 0 14px 28px -22px ${withAlpha(BLACK, 0.72)}`,
      shadowStrength,
    ),
    inputActiveShadow: `0 0 0 3px ${inputOutlineColor}`,
    inputErrorActiveShadow: `0 0 0 3px ${withAlpha(error.color, 0.28)}`,
    layoutSiderShadow: scaleShadowAlpha(`2px 0 18px -14px ${withAlpha(BLACK, 0.72)}`, shadowStrength),
    scrollbarThumbBg: mix(surfaces.colorBorderSecondary, surfaces.colorTextSecondary, 0.16),
    scrollbarThumbHoverBg: mix(surfaces.colorBorderSecondary, surfaces.colorTextSecondary, 0.3),
  }
}

function deriveLightComponentMap(
  surfaces: ThemeSurfaceMap,
  aliases: ThemeAliasMap,
  primary: ThemePrimaryMap,
  error: FunctionalColorMap,
  shadowStrength: number,
): ThemeComponentMap {
  const inputOutlineColor = 'var(--cp-color-primary-bg-hover)'
  const appearanceInfluence = deriveComponentAppearanceInfluence(
    surfaces.colorBgContainer,
    LIGHT_CONTAINER_BASE,
    surfaces.colorText,
    LIGHT_TEXT_BASE,
  )

  return {
    menuItemSelectedBg: surfaces.colorBgTextActive,
    inputBg: mix(surfaces.colorBgContainer, surfaces.colorFillSecondary, 0.78),
    inputHoverBg: mix(
      LIGHT_COMPONENT_ANCHORS.inputHoverBg,
      mix(surfaces.colorBgContainer, surfaces.colorFillTertiary, 0.88),
      appearanceInfluence,
    ),
    inputActiveBg: mix(
      LIGHT_COMPONENT_ANCHORS.inputActiveBg,
      aliases.colorFillAlter,
      appearanceInfluence,
    ),
    inputErrorActiveBg: error.background,
    buttonPrimaryColor: primary.colorTextLightSolid,
    buttonPrimaryBg: primary.colorPrimary,
    buttonPrimaryHoverBg: mix(primary.colorPrimary, WHITE, 0.06),
    buttonPrimaryActiveBg: mix(primary.colorPrimary, BLACK, 0.08),
    brandMarkBg: surfaces.colorBgSpotlight,
    cardBg: surfaces.colorBgContainer,
    modalBg: surfaces.colorBgContainer,
    tableHeaderBg: surfaces.colorFillTertiary,
    tableRowBg: surfaces.colorBgContainer,
    tableRowStripeBg: aliases.colorFillAlter,
    tableRowHoverBg: surfaces.colorBgTextHover,
    tableRowSelectedBg: primary.colorPrimaryBg,
    progressRemainingColor: surfaces.colorBorderSecondary,
    layoutSiderBg: surfaces.colorBgContainer,
    cardShadow: scaleShadowAlpha(`0 10px 22px -18px ${withAlpha(LIGHT_SHADOW_BASE, 0.08)}`, shadowStrength),
    inputShadow: scaleShadowAlpha(`0 9px 18px -14px ${withAlpha(LIGHT_SHADOW_BASE, 0.086)}`, shadowStrength),
    inputHoverShadow: scaleShadowAlpha(
      `0 0 0 3px ${inputOutlineColor}, 0 12px 24px -16px ${withAlpha(LIGHT_SHADOW_BASE, 0.12)}`,
      shadowStrength,
    ),
    inputActiveShadow: `0 0 0 3px ${inputOutlineColor}`,
    inputErrorActiveShadow: `0 0 0 3px ${withAlpha(error.color, 0.18)}`,
    layoutSiderShadow: scaleShadowAlpha(`2px 0 12px -12px ${withAlpha(LIGHT_SHADOW_BASE, 0.027)}`, shadowStrength),
    scrollbarThumbBg: mix(surfaces.colorBgLayout, surfaces.colorTextSecondary, 0.2),
    scrollbarThumbHoverBg: mix(surfaces.colorBgLayout, surfaces.colorTextSecondary, 0.34),
  }
}

function deriveComponentAppearanceInfluence(
  background: string,
  baselineBackground: string,
  text: string,
  baselineText: string,
): number {
  const distance = Math.min(
    1,
    Math.max(
      relativeColorDistance(background, baselineBackground),
      relativeColorDistance(text, baselineText),
    ) / FULL_COMPONENT_APPEARANCE_DISTANCE,
  )
  return distance * distance * (3 - 2 * distance)
}
