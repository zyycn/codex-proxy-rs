import type {
  FunctionalColorMap,
  ResolvedThemeSeedTokens,
  ThemeComponentMap,
  ThemeComponentOverrides,
  ThemeDimensionMap,
  ThemeFillMap,
  ThemeName,
  ThemePrimaryMap,
  ThemeShadowMap,
  ThemeSurfaceMap,
} from '../types'
import {
  compositeOnBackground,
  ensureContrast,
  mix,
  scaleShadowAlpha,
  withAlpha,
} from '../core/color'
import {
  BLACK,
  DEFAULT_DIMENSIONS,
  LIGHT_SHADOW_BASE,
} from '../core/constants'

type SurfaceComponentMap = Omit<ThemeComponentMap, `buttonPrimary${string}` | 'tableRowSelectedHoverBg'>

const SHADOW_MAP_DERIVERS = {
  light: deriveLightShadowMap,
  dark: deriveDarkShadowMap,
} satisfies Record<ThemeName, (shadowStrength: number) => ThemeShadowMap>

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
  surfaces: ThemeSurfaceMap,
  fills: ThemeFillMap | null,
  primary: ThemePrimaryMap,
  error: FunctionalColorMap,
  shadowStrength: number,
): ThemeComponentMap {
  // 明暗模式共用组件合同，各自保留适合宿主表面的填充配方。
  const components = fills
    ? deriveLightComponentMap(surfaces, fills, primary, error, shadowStrength)
    : deriveDarkComponentMap(surfaces, primary, error, shadowStrength)
  const buttonPrimaryBg = ensureContrast(primary.colorPrimary, primary.colorTextLightSolid, 4.5)
  return {
    ...components,
    // 暗色选中行只轻微提亮，保留原有蓝灰色温和选中语义。
    tableRowSelectedHoverBg: fills
      ? primary.colorPrimaryContainerHover
      : mix(primary.colorPrimaryContainer, surfaces.colorText, 0.04),
    buttonPrimaryColor: primary.colorTextLightSolid,
    buttonPrimaryBg,
    buttonPrimaryHoverBg: mix(buttonPrimaryBg, BLACK, 0.08),
    buttonPrimaryActiveBg: mix(buttonPrimaryBg, BLACK, 0.16),
  }
}

function deriveDarkComponentMap(
  surfaces: ThemeSurfaceMap,
  primary: ThemePrimaryMap,
  error: FunctionalColorMap,
  shadowStrength: number,
): SurfaceComponentMap {
  const inputInteractionBg = mix(surfaces.colorBgContainer, surfaces.colorFillSecondary, 0.69)
  // 悬停与聚焦共用反馈；装饰阴影关闭时仍保留可见外圈。
  const inputInteractionShadow = [
    '0 0 0 3px var(--cp-color-primary-container-hover)',
    shadowStrength > 0
      ? scaleShadowAlpha(`0 14px 28px -22px ${withAlpha(BLACK, 0.72)}`, shadowStrength)
      : null,
  ].filter(Boolean).join(', ')

  return {
    buttonSecondaryBg: surfaces.colorFillTertiary,
    buttonSecondaryHoverBg: surfaces.colorBgTextActive,
    buttonSecondaryActiveBg: surfaces.colorBgTextActive,
    iconButtonSecondaryBg: surfaces.colorBgContainer,
    iconButtonSecondaryHoverBg: surfaces.colorBgTextHover,
    iconButtonSecondaryActiveBg: surfaces.colorBgTextActive,
    menuItemSelectedBg: surfaces.colorBgTextHover,
    inputBg: mix(surfaces.colorBgContainer, surfaces.colorFillSecondary, 0.375),
    inputHoverBg: inputInteractionBg,
    inputActiveBg: inputInteractionBg,
    inputErrorActiveBg: error.container,
    brandMarkBg: surfaces.colorBgElevated,
    cardBg: surfaces.colorBgContainer,
    modalBg: surfaces.colorBgContainer,
    popoverHeaderBg: surfaces.colorFillTertiary,
    tableHeaderBg: surfaces.colorFillTertiary,
    tableRowBg: surfaces.colorBgContainer,
    tableRowStripeBg: surfaces.colorFillQuaternary,
    tableRowHoverBg: surfaces.colorBgTextHover,
    tableRowSelectedBg: primary.colorPrimaryContainer,
    progressRemainingColor: mix(surfaces.colorBgContainer, surfaces.colorBorderSecondary, 0.92),
    layoutSiderBg: surfaces.colorBgContainer,
    cardShadow: scaleShadowAlpha(`0 18px 34px -26px ${withAlpha(BLACK, 0.64)}`, shadowStrength),
    inputShadow: scaleShadowAlpha(`0 12px 24px -20px ${withAlpha(BLACK, 0.66)}`, shadowStrength),
    inputHoverShadow: inputInteractionShadow,
    inputActiveShadow: inputInteractionShadow,
    inputErrorActiveShadow: `0 0 0 3px ${withAlpha(error.color, 0.28)}`,
    layoutSiderShadow: scaleShadowAlpha(`2px 0 18px -14px ${withAlpha(BLACK, 0.72)}`, shadowStrength),
    scrollbarThumbBg: mix(surfaces.colorBorderSecondary, surfaces.colorTextSecondary, 0.16),
    scrollbarThumbHoverBg: mix(surfaces.colorBorderSecondary, surfaces.colorTextSecondary, 0.3),
  }
}

function deriveLightComponentMap(
  surfaces: ThemeSurfaceMap,
  fills: ThemeFillMap,
  primary: ThemePrimaryMap,
  error: FunctionalColorMap,
  shadowStrength: number,
): SurfaceComponentMap {
  const inputInteractionShadow = `0 0 0 3px ${withAlpha(primary.colorPrimary, 0.16)}`
  return {
    buttonSecondaryBg: fills.controlBg,
    buttonSecondaryHoverBg: fills.controlHoverBg,
    buttonSecondaryActiveBg: fills.controlActiveBg,
    iconButtonSecondaryBg: fills.controlBg,
    iconButtonSecondaryHoverBg: fills.controlHoverBg,
    iconButtonSecondaryActiveBg: fills.controlActiveBg,
    inputBg: fills.controlBg,
    inputHoverBg: surfaces.colorBgContainer,
    inputActiveBg: surfaces.colorBgContainer,
    inputErrorActiveBg: error.container,
    inputShadow: 'none',
    inputHoverShadow: inputInteractionShadow,
    inputActiveShadow: inputInteractionShadow,
    inputErrorActiveShadow: `0 0 0 3px ${withAlpha(error.color, 0.18)}`,
    // 先合成实色，固定列滚动时不会透出下层内容。
    tableHeaderBg: compositeOnBackground(fills.contentBg, surfaces.colorBgContainer),
    tableRowBg: surfaces.colorBgContainer,
    tableRowStripeBg: compositeOnBackground(fills.contentStripeBg, surfaces.colorBgContainer),
    tableRowHoverBg: compositeOnBackground(fills.contentHoverBg, surfaces.colorBgContainer),
    tableRowSelectedBg: primary.colorPrimaryContainer,
    menuItemSelectedBg: surfaces.colorBgTextActive,
    brandMarkBg: surfaces.colorBgSpotlight,
    cardBg: surfaces.colorBgContainer,
    modalBg: surfaces.colorBgContainer,
    popoverHeaderBg: surfaces.colorFillSecondary,
    progressRemainingColor: surfaces.colorBorderSecondary,
    layoutSiderBg: surfaces.colorBgContainer,
    cardShadow: scaleShadowAlpha(`0 10px 22px -18px ${withAlpha(LIGHT_SHADOW_BASE, 0.08)}`, shadowStrength),
    layoutSiderShadow: scaleShadowAlpha(`2px 0 12px -12px ${withAlpha(LIGHT_SHADOW_BASE, 0.027)}`, shadowStrength),
    scrollbarThumbBg: mix(surfaces.colorBgLayout, surfaces.colorTextSecondary, 0.2),
    scrollbarThumbHoverBg: mix(surfaces.colorBgLayout, surfaces.colorTextSecondary, 0.34),
  }
}
