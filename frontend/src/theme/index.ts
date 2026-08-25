import { generate } from '@ant-design/colors'

import { normalizeHexColor } from '@/utils/color'

import {
  BLACK,
  DARK_CONTAINER_BASE,
  DARK_TEXT_BASE,
  DEFAULT_CUSTOM_THEME_COLOR,
  DEFAULT_DIMENSIONS,
  DEFAULT_SEED_COLORS,
  DEFAULT_THEME_CUSTOMIZATION,
  EDITABLE_COLOR_TOKEN_NAMES,
  EDITABLE_SHADOW_TOKEN_NAMES,
  LIGHT_CONTAINER_BASE,
  LIGHT_FOREGROUND,
  LIGHT_TEXT_BASE,
  NEUTRAL_DARK_INPUT,
  NEUTRAL_DARK_SEMANTIC_TEXT,
  NEUTRAL_DARK_SHADOWS,
  NEUTRAL_DARK_SURFACES,
  NEUTRAL_LIGHT_INPUT,
  NEUTRAL_LIGHT_SHADOWS,
  NEUTRAL_LIGHT_SURFACES,
  THEME_COLOR_PRESETS,
  WHITE,
} from './constants'

export { applyResolvedTheme } from './browser'
export {
  DEFAULT_CUSTOM_THEME_COLOR,
  DEFAULT_THEME_COLOR,
  DEFAULT_THEME_CUSTOMIZATION,
  DEFAULT_THEME_MODE,
  THEME_COLOR_PRESETS,
} from './constants'

export type ThemeMode = 'system' | 'light' | 'dark'
export type ThemeName = 'light' | 'dark'
export type ThemeColorPresetId = 'relay-blue' | 'deep-teal' | 'signal-violet' | 'graphite'
export type ThemeColorId = ThemeColorPresetId | 'custom'

export interface ThemeAppearanceSeed {
  colorBgBase: string
  colorTextBase: string
}

export interface ThemeColorPreset {
  id: ThemeColorPresetId
  label: string
  description: string
  seed: string
  appearance?: Partial<Record<ThemeName, ThemeAppearanceSeed>>
}

export type ThemeTokenName
  = '--cp-color-bg-layout'
    | '--cp-color-bg-container'
    | '--cp-color-bg-elevated'
    | '--cp-color-bg-spotlight'
    | '--cp-color-bg-mask'
    | '--cp-color-bg-text-hover'
    | '--cp-color-bg-text-active'
    | '--cp-color-bg-container-disabled'
    | '--cp-color-fill-secondary'
    | '--cp-color-fill-tertiary'
    | '--cp-color-fill-quaternary'
    | '--cp-color-border'
    | '--cp-color-border-secondary'
    | '--cp-color-split'
    | '--cp-color-text'
    | '--cp-color-text-heading'
    | '--cp-color-text-secondary'
    | '--cp-color-text-tertiary'
    | '--cp-color-text-quaternary'
    | '--cp-color-text-disabled'
    | '--cp-color-text-light-solid'
    | '--cp-color-shadow'
    | '--cp-color-primary-bg'
    | '--cp-color-primary-bg-hover'
    | '--cp-color-primary-border'
    | '--cp-color-primary-border-hover'
    | '--cp-color-primary-hover'
    | '--cp-color-primary'
    | '--cp-color-primary-active'
    | '--cp-color-primary-text-hover'
    | '--cp-color-primary-text'
    | '--cp-color-primary-text-active'
    | '--cp-color-link'
    | '--cp-color-link-hover'
    | '--cp-color-link-active'
    | '--cp-control-item-bg-active'
    | '--cp-control-item-bg-active-hover'
    | '--cp-control-outline'
    | '--cp-menu-item-selected-bg'
    | '--cp-input-bg'
    | '--cp-input-hover-bg'
    | '--cp-input-active-bg'
    | '--cp-input-error-active-bg'
    | '--cp-button-primary-color'
    | '--cp-button-primary-bg'
    | '--cp-button-primary-hover-bg'
    | '--cp-button-primary-active-bg'
    | '--cp-brand-mark-bg'
    | '--cp-card-bg'
    | '--cp-table-header-bg'
    | '--cp-table-row-bg'
    | '--cp-table-row-stripe-bg'
    | '--cp-table-row-hover-bg'
    | '--cp-table-row-selected-bg'
    | '--cp-progress-remaining-color'
    | '--cp-layout-sider-bg'
    | '--cp-box-shadow'
    | '--cp-box-shadow-secondary'
    | '--cp-box-shadow-tertiary'
    | '--cp-card-shadow'
    | '--cp-input-shadow'
    | '--cp-input-hover-shadow'
    | '--cp-input-active-shadow'
    | '--cp-input-error-active-shadow'
    | '--cp-layout-sider-shadow'
    | '--cp-scrollbar-thumb-bg'
    | '--cp-scrollbar-thumb-hover-bg'
    | '--cp-color-info'
    | '--cp-color-info-hover'
    | '--cp-color-info-active'
    | '--cp-color-info-bg'
    | '--cp-color-info-border'
    | '--cp-color-info-text'
    | '--cp-color-success'
    | '--cp-color-success-bg'
    | '--cp-color-success-bg-hover'
    | '--cp-color-success-bg-active'
    | '--cp-color-success-border'
    | '--cp-color-success-text'
    | '--cp-color-warning'
    | '--cp-color-warning-bg'
    | '--cp-color-warning-bg-hover'
    | '--cp-color-warning-bg-active'
    | '--cp-color-warning-border'
    | '--cp-color-warning-text'
    | '--cp-color-error'
    | '--cp-color-error-bg'
    | '--cp-color-error-bg-hover'
    | '--cp-color-error-bg-active'
    | '--cp-color-error-border'
    | '--cp-color-error-text'
    | '--cp-control-height-sm'
    | '--cp-control-height'
    | '--cp-control-height-lg'
    | '--cp-font-size-xs'
    | '--cp-font-size-sm'
    | '--cp-font-size'
    | '--cp-font-size-lg'
    | '--cp-font-size-xl'
    | '--cp-line-height-xs'
    | '--cp-line-height-sm'
    | '--cp-line-height'
    | '--cp-line-height-lg'
    | '--cp-line-height-xl'
    | '--cp-size-unit'
    | '--cp-size-step'
    | '--cp-table-row-height'
    | '--cp-table-row-height-sm'
    | '--cp-border-radius-sm'
    | '--cp-border-radius'
    | '--cp-border-radius-lg'
    | '--cp-card-border-radius'

export type ThemeTokens = Record<ThemeTokenName, string>

export interface ThemeSeedOverrides {
  colorSuccess?: string
  colorWarning?: string
  colorError?: string
  colorInfo?: string
  colorLink?: string
  colorTextBase?: string
  colorBgBase?: string
  fontSize?: number
  sizeUnit?: number
  sizeStep?: number
  controlHeight?: number
  borderRadius?: number
  shadowStrength?: number
}

export interface ThemeComponentOverrides {
  tableRowHeight?: number
  cardBorderRadius?: number
}

export interface ThemeCustomization {
  seed?: ThemeSeedOverrides
  component?: ThemeComponentOverrides
  tokenOverrides?: Partial<ThemeTokens>
}

export interface ResolvedThemeSeedTokens {
  colorPrimary: string
  colorSuccess: string
  colorWarning: string
  colorError: string
  colorInfo: string
  colorLink: string
  colorTextBase: string
  colorBgBase: string
  fontSize: number
  sizeUnit: number
  sizeStep: number
  controlHeight: number
  borderRadius: number
  shadowStrength: number
}

export interface ResolvedTheme {
  name: ThemeName
  colorId: ThemeColorId
  seed: string
  seedTokens: ResolvedThemeSeedTokens
  tokens: ThemeTokens
}

interface RgbColor {
  red: number
  green: number
  blue: number
}

/** Seed 派生出的主色 Map。页面和组件不得直接消费这一层。 */
interface ThemePrimaryMap {
  colorPrimaryBg: string
  colorPrimaryBgHover: string
  colorPrimaryBorder: string
  colorPrimaryBorderHover: string
  colorPrimaryHover: string
  colorPrimary: string
  colorPrimaryActive: string
  colorPrimaryTextHover: string
  colorPrimaryText: string
  colorPrimaryTextActive: string
  colorPrimarySolid: string
  colorPrimarySolidHover: string
  colorPrimarySolidActive: string
  colorTextLightSolid: string
}

/** 背景与文字 Seed 派生出的中性表面 Map；品牌主色不得进入这一层。 */
interface ThemeSurfaceMap {
  colorBgLayout: string
  colorBgContainer: string
  colorBgElevated: string
  colorBgSpotlight: string
  colorBgMask: string
  colorBgTextHover: string
  colorBgTextActive: string
  colorBgContainerDisabled: string
  colorFillSecondary: string
  colorFillTertiary: string
  colorFillQuaternary: string
  colorBorder: string
  colorBorderSecondary: string
  colorSplit: string
  colorText: string
  colorTextHeading: string
  colorTextSecondary: string
  colorTextTertiary: string
  colorTextQuaternary: string
  colorTextDisabled: string
  colorShadow: string
}

interface ThemeShadowMap {
  boxShadow: string
  boxShadowSecondary: string
  boxShadowTertiary: string
}

interface ThemeComponentMap {
  menuItemSelectedBg: string
  inputBg: string
  inputHoverBg: string
  inputActiveBg: string
  inputErrorActiveBg: string
  buttonPrimaryColor: string
  buttonPrimaryBg: string
  buttonPrimaryHoverBg: string
  buttonPrimaryActiveBg: string
  brandMarkBg: string
  cardBg: string
  tableHeaderBg: string
  tableRowBg: string
  tableRowStripeBg: string
  tableRowHoverBg: string
  tableRowSelectedBg: string
  progressRemainingColor: string
  layoutSiderBg: string
  cardShadow: string
  inputShadow: string
  inputHoverShadow: string
  inputActiveShadow: string
  inputErrorActiveShadow: string
  layoutSiderShadow: string
  scrollbarThumbBg: string
  scrollbarThumbHoverBg: string
}

interface FunctionalColorMap {
  color: string
  hover: string
  active: string
  background: string
  backgroundHover: string
  backgroundActive: string
  border: string
  text: string
}

interface ThemeSemanticMap {
  info: FunctionalColorMap
  success: FunctionalColorMap
  warning: FunctionalColorMap
  error: FunctionalColorMap
}

interface ThemeDimensionMap {
  fontSizeXs: string
  fontSizeSm: string
  fontSize: string
  fontSizeLg: string
  fontSizeXl: string
  lineHeightXs: string
  lineHeightSm: string
  lineHeight: string
  lineHeightLg: string
  lineHeightXl: string
  sizeUnit: string
  sizeStep: string
  controlHeightSm: string
  controlHeight: string
  controlHeightLg: string
  tableRowHeight: string
  tableRowHeightSm: string
  borderRadiusSm: string
  borderRadius: string
  borderRadiusLg: string
  cardBorderRadius: string
}

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
  const surfaces = deriveThemeSurfaceMap(name, seedTokens)
  const primary = deriveThemePrimaryMap(seed, name, surfaces.colorBgContainer)
  const link = seedTokens.colorLink === seed
    ? primary
    : deriveThemePrimaryMap(seedTokens.colorLink, name, surfaces.colorBgContainer)
  const semantics = deriveThemeSemanticMap(name, surfaces.colorBgContainer, seedTokens)
  const shadows = deriveThemeShadowMap(name, seedTokens.shadowStrength)
  const components = deriveThemeComponentMap(
    name,
    surfaces,
    primary,
    semantics.error,
    seedTokens.shadowStrength,
  )
  const dimensions = deriveThemeDimensionMap(seedTokens, normalizedCustomization.component)
  const tokens = toThemeTokens(surfaces, primary, link, semantics, shadows, components, dimensions)
  const mergedTokens: ThemeTokens = {
    ...tokens,
    ...normalizedCustomization.tokenOverrides,
  }
  mergedTokens['--cp-button-primary-color']
    = normalizedCustomization.tokenOverrides?.['--cp-button-primary-color']
      ?? solidControlTextColor(mergedTokens['--cp-button-primary-bg'])

  return {
    name,
    colorId,
    seed,
    seedTokens,
    tokens: mergedTokens,
  }
}

export function normalizeThemeCustomization(value: unknown): ThemeCustomization {
  if (!value || typeof value !== 'object')
    return {}

  const source = value as { seed?: unknown, component?: unknown, tokenOverrides?: unknown }
  const seed = normalizeThemeSeedOverrides(source.seed)
  const component = normalizeThemeComponentOverrides(source.component)
  const tokenOverrides = normalizeThemeTokenOverrides(source.tokenOverrides)
  return {
    ...(Object.keys(seed).length > 0 ? { seed } : {}),
    ...(Object.keys(component).length > 0 ? { component } : {}),
    ...(Object.keys(tokenOverrides).length > 0 ? { tokenOverrides } : {}),
  }
}

export function themeColorPreset(id: ThemeColorPresetId): ThemeColorPreset {
  return THEME_COLOR_PRESETS.find(preset => preset.id === id) ?? THEME_COLOR_PRESETS[0]!
}

export function isThemeMode(value: unknown): value is ThemeMode {
  return value === 'system' || value === 'light' || value === 'dark'
}

export function isThemeColorId(value: unknown): value is ThemeColorId {
  return value === 'custom' || THEME_COLOR_PRESETS.some(preset => preset.id === value)
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

/**
 * 自定义品牌色只为中性界面注入少量色温，再进入统一 Surface Map。
 * 主色负责交互与状态反馈，不能直接把所有容器和浮层染成同一块高色度表面。
 * 显式的背景或文字 Seed 会在上一层覆盖这里。
 */
function deriveCustomThemeAppearance(
  theme: ThemeName,
  colorPrimary: string,
): ThemeAppearanceSeed {
  if (theme === 'dark') {
    const [paletteBackground] = generate(colorPrimary, { theme: 'dark' })
    const tintedBackground = normalizeHexColor(paletteBackground) ?? DARK_CONTAINER_BASE
    return {
      colorBgBase: mix(DARK_CONTAINER_BASE, tintedBackground, 0.3),
      colorTextBase: DARK_TEXT_BASE,
    }
  }

  return {
    colorBgBase: mix(LIGHT_CONTAINER_BASE, colorPrimary, 0.028),
    colorTextBase: LIGHT_TEXT_BASE,
  }
}

function deriveThemeSurfaceMap(
  theme: ThemeName,
  seedTokens: ResolvedThemeSeedTokens,
): ThemeSurfaceMap {
  return theme === 'dark'
    ? deriveNeutralDarkSurfaceMap(seedTokens)
    : deriveNeutralLightSurfaceMap(seedTokens)
}

function deriveNeutralDarkSurfaceMap(seedTokens: ResolvedThemeSeedTokens): ThemeSurfaceMap {
  const background = seedTokens.colorBgBase
  const baselineBackground = background === DARK_CONTAINER_BASE
  const colorText = seedTokens.colorTextBase
  const colorFillQuaternary = baselineBackground
    ? NEUTRAL_DARK_SURFACES.colorFillQuaternary
    : mix(background, colorText, 0.045)
  const colorFillTertiary = baselineBackground
    ? NEUTRAL_DARK_SURFACES.colorFillTertiary
    : mix(background, colorText, 0.075)
  const colorFillSecondary = baselineBackground
    ? NEUTRAL_DARK_SURFACES.colorFillSecondary
    : mix(background, colorText, 0.12)
  const colorBorderSecondary = baselineBackground
    ? NEUTRAL_DARK_SURFACES.colorBorderSecondary
    : mix(background, colorText, 0.105)
  const colorBorder = baselineBackground
    ? NEUTRAL_DARK_SURFACES.colorBorder
    : mix(background, colorText, 0.155)
  const colorBgTextHover = baselineBackground
    ? NEUTRAL_DARK_SURFACES.colorBgTextHover
    : mix(background, colorText, 0.08)
  const baselineText = colorText === DARK_TEXT_BASE

  return {
    colorBgLayout: baselineBackground
      ? NEUTRAL_DARK_SURFACES.colorBgLayout
      : mix(background, BLACK, 0.3),
    colorBgContainer: background,
    colorBgElevated: baselineBackground
      ? NEUTRAL_DARK_SURFACES.colorBgElevated
      : mix(background, WHITE, 0.045),
    colorBgSpotlight: baselineBackground
      ? NEUTRAL_DARK_SURFACES.colorBgSpotlight
      : mix(background, BLACK, 0.74),
    colorBgMask: baselineBackground
      ? NEUTRAL_DARK_SURFACES.colorBgMask
      : withAlpha(mix(background, BLACK, 0.8), 0.68),
    // 静态弱填充比 Hover 更安静，避免所有内容块都呈现为交互态。
    colorBgTextHover,
    colorBgTextActive: baselineBackground
      ? NEUTRAL_DARK_SURFACES.colorBgTextActive
      : colorFillSecondary,
    colorBgContainerDisabled: baselineBackground
      ? NEUTRAL_DARK_SURFACES.colorBgContainerDisabled
      : mix(background, colorText, 0.035),
    colorFillSecondary,
    colorFillTertiary,
    colorFillQuaternary,
    colorBorder,
    colorBorderSecondary,
    colorSplit: baselineBackground
      ? NEUTRAL_DARK_SURFACES.colorSplit
      : withAlpha(colorBorderSecondary, 0.5),
    colorText,
    colorTextHeading: baselineText
      ? NEUTRAL_DARK_SURFACES.colorTextHeading
      : mix(colorText, WHITE, 0.74),
    colorTextSecondary: baselineText
      ? NEUTRAL_DARK_SURFACES.colorTextSecondary
      : mix(colorText, background, 0.36),
    colorTextTertiary: baselineText
      ? NEUTRAL_DARK_SURFACES.colorTextTertiary
      : mix(colorText, background, 0.5),
    colorTextQuaternary: baselineText
      ? NEUTRAL_DARK_SURFACES.colorTextQuaternary
      : mix(colorText, background, 0.58),
    colorTextDisabled: baselineText
      ? NEUTRAL_DARK_SURFACES.colorTextDisabled
      : mix(colorText, background, 0.66),
    colorShadow: withAlpha(BLACK, 0.75),
  }
}

function deriveNeutralLightSurfaceMap(seedTokens: ResolvedThemeSeedTokens): ThemeSurfaceMap {
  const background = seedTokens.colorBgBase
  const baselineBackground = background === LIGHT_CONTAINER_BASE
  const colorFillQuaternary = baselineBackground
    ? NEUTRAL_LIGHT_SURFACES.colorFillQuaternary
    : mix(background, seedTokens.colorTextBase, 0.035)
  const colorFillTertiary = baselineBackground
    ? NEUTRAL_LIGHT_SURFACES.colorFillTertiary
    : mix(background, seedTokens.colorTextBase, 0.06)
  const colorFillSecondary = baselineBackground
    ? NEUTRAL_LIGHT_SURFACES.colorFillSecondary
    : mix(background, seedTokens.colorTextBase, 0.09)
  const colorBorderSecondary = baselineBackground
    ? NEUTRAL_LIGHT_SURFACES.colorBorderSecondary
    : mix(background, seedTokens.colorTextBase, 0.105)
  const colorBorder = baselineBackground
    ? NEUTRAL_LIGHT_SURFACES.colorBorder
    : mix(background, seedTokens.colorTextBase, 0.145)
  const colorBgTextHover = baselineBackground
    ? NEUTRAL_LIGHT_SURFACES.colorBgTextHover
    : mix(background, seedTokens.colorTextBase, 0.06)
  const colorText = seedTokens.colorTextBase
  const baselineText = colorText === LIGHT_TEXT_BASE

  return {
    colorBgLayout: baselineBackground
      ? NEUTRAL_LIGHT_SURFACES.colorBgLayout
      : mix(background, colorText, 0.035),
    colorBgContainer: background,
    colorBgElevated: baselineBackground
      ? NEUTRAL_LIGHT_SURFACES.colorBgElevated
      : mix(background, colorText, 0.018),
    colorBgSpotlight: baselineBackground
      ? NEUTRAL_LIGHT_SURFACES.colorBgSpotlight
      : mix(colorText, BLACK, 0.08),
    colorBgMask: baselineBackground
      ? NEUTRAL_LIGHT_SURFACES.colorBgMask
      : withAlpha(colorText, 0.3),
    colorBgTextHover,
    colorBgTextActive: baselineBackground
      ? NEUTRAL_LIGHT_SURFACES.colorBgTextActive
      : colorFillSecondary,
    colorBgContainerDisabled: baselineBackground
      ? NEUTRAL_LIGHT_SURFACES.colorBgContainerDisabled
      : colorFillTertiary,
    colorFillSecondary,
    colorFillTertiary,
    colorFillQuaternary,
    colorBorder,
    colorBorderSecondary,
    colorSplit: baselineBackground
      ? NEUTRAL_LIGHT_SURFACES.colorSplit
      : withAlpha(colorBorder, 0.42),
    colorText,
    colorTextHeading: baselineText
      ? NEUTRAL_LIGHT_SURFACES.colorTextHeading
      : mix(colorText, BLACK, 0.18),
    colorTextSecondary: baselineText
      ? NEUTRAL_LIGHT_SURFACES.colorTextSecondary
      : mix(colorText, background, 0.36),
    colorTextTertiary: baselineText
      ? NEUTRAL_LIGHT_SURFACES.colorTextTertiary
      : mix(colorText, background, 0.55),
    colorTextQuaternary: baselineText
      ? NEUTRAL_LIGHT_SURFACES.colorTextQuaternary
      : mix(colorText, background, 0.6),
    colorTextDisabled: baselineText
      ? NEUTRAL_LIGHT_SURFACES.colorTextDisabled
      : mix(colorText, background, 0.62),
    colorShadow: withAlpha(colorText, 0.38),
  }
}

function deriveThemePrimaryMap(
  seed: string,
  theme: ThemeName,
  containerBg: string,
): ThemePrimaryMap {
  const palette = generate(seed, theme === 'dark'
    ? { theme: 'dark', backgroundColor: containerBg }
    : undefined).map(color => normalizeHexColor(color) ?? DEFAULT_CUSTOM_THEME_COLOR)
  const primaryIndex = theme === 'dark' ? 6 : 5
  const hoverIndex = theme === 'dark' ? 7 : 4
  const activeIndex = theme === 'dark' ? 5 : 6
  const colorPrimary = normalizeHexColor(seed) ?? DEFAULT_CUSTOM_THEME_COLOR
  const colorPrimaryHover = palette[hoverIndex]!
  const colorPrimaryActive = palette[activeIndex]!
  const colorPrimarySolid = colorPrimary
  const colorPrimarySolidHover = mix(colorPrimarySolid, WHITE, 0.06)
  const colorPrimarySolidActive = mix(colorPrimarySolid, BLACK, 0.08)
  const colorPrimaryText = ensureContrast(palette[primaryIndex]!, containerBg, 4.5)
  const colorPrimaryTextHover = ensureContrast(palette[hoverIndex]!, containerBg, 4.5)
  const colorPrimaryTextActive = ensureContrast(palette[activeIndex]!, containerBg, 4.5)
  const borderMix = theme === 'dark' ? 0.52 : 0.46
  const borderHoverMix = theme === 'dark' ? 0.62 : 0.56
  const backgroundMix = theme === 'dark' ? 0.17 : 0.09
  const backgroundHoverMix = theme === 'dark' ? 0.24 : 0.14

  return {
    // 任意深浅 Seed 的弱背景都从当前 surface 混合，避免低明度 Seed 生成脏灰色块。
    colorPrimaryBg: mix(containerBg, colorPrimary, backgroundMix),
    colorPrimaryBgHover: mix(containerBg, colorPrimaryHover, backgroundHoverMix),
    colorPrimaryBorder: ensureContrast(mix(containerBg, colorPrimary, borderMix), containerBg, 3),
    colorPrimaryBorderHover: ensureContrast(
      mix(containerBg, colorPrimaryHover, borderHoverMix),
      containerBg,
      3,
    ),
    colorPrimaryHover,
    colorPrimary,
    colorPrimaryActive,
    colorPrimaryTextHover,
    colorPrimaryText,
    colorPrimaryTextActive,
    colorPrimarySolid,
    colorPrimarySolidHover,
    colorPrimarySolidActive,
    colorTextLightSolid: LIGHT_FOREGROUND,
  }
}

function deriveThemeSemanticMap(
  theme: ThemeName,
  containerBg: string,
  seedTokens: ResolvedThemeSeedTokens,
): ThemeSemanticMap {
  const usesNeutralDarkText = theme === 'dark' && containerBg === DARK_CONTAINER_BASE

  return {
    info: deriveFunctionalColorMap(
      seedTokens.colorInfo,
      theme,
      containerBg,
      usesNeutralDarkText && normalizeHexColor(seedTokens.colorInfo) === DEFAULT_SEED_COLORS.colorInfo
        ? NEUTRAL_DARK_SEMANTIC_TEXT.info
        : undefined,
    ),
    success: deriveFunctionalColorMap(
      seedTokens.colorSuccess,
      theme,
      containerBg,
      usesNeutralDarkText && normalizeHexColor(seedTokens.colorSuccess) === DEFAULT_SEED_COLORS.colorSuccess
        ? NEUTRAL_DARK_SEMANTIC_TEXT.success
        : undefined,
    ),
    warning: deriveFunctionalColorMap(
      seedTokens.colorWarning,
      theme,
      containerBg,
      usesNeutralDarkText && normalizeHexColor(seedTokens.colorWarning) === DEFAULT_SEED_COLORS.colorWarning
        ? NEUTRAL_DARK_SEMANTIC_TEXT.warning
        : undefined,
    ),
    error: deriveFunctionalColorMap(
      seedTokens.colorError,
      theme,
      containerBg,
      usesNeutralDarkText && normalizeHexColor(seedTokens.colorError) === DEFAULT_SEED_COLORS.colorError
        ? NEUTRAL_DARK_SEMANTIC_TEXT.error
        : undefined,
    ),
  }
}

function deriveFunctionalColorMap(
  seed: string,
  theme: ThemeName,
  containerBg: string,
  baselineText?: string,
): FunctionalColorMap {
  const palette = generate(seed, theme === 'dark'
    ? { theme: 'dark', backgroundColor: containerBg }
    : undefined).map(color => normalizeHexColor(color) ?? seed)
  // 与 Ant Design 的功能色 Map 保持一致：基础功能色固定取 P6。
  // 浅色 P6 即用户选择的 Seed；深色 P6 由暗色色板适配。对比度保护只作用于具体角色 Token。
  const color = palette[5]!
  const hover = palette[theme === 'dark' ? 7 : 4]!
  const active = palette[theme === 'dark' ? 5 : 6]!
  const backgroundMix = theme === 'dark' ? 0.18 : 0.08
  const backgroundHoverMix = theme === 'dark' ? 0.24 : 0.12
  const backgroundActiveMix = theme === 'dark' ? 0.3 : 0.18
  const text = baselineText ?? palette[theme === 'dark' ? 8 : 5]!

  return {
    color,
    hover: ensureContrast(hover, containerBg, 3),
    active: ensureContrast(active, containerBg, 3),
    background: mix(containerBg, color, backgroundMix),
    backgroundHover: mix(containerBg, hover, backgroundHoverMix),
    backgroundActive: mix(containerBg, active, backgroundActiveMix),
    border: ensureContrast(mix(containerBg, color, theme === 'dark' ? 0.46 : 0.4), containerBg, 3),
    // 暗色语义文字使用更亮的色阶，避免把实心状态色直接当作小字号文字色。
    text: ensureContrast(text, containerBg, 4.5),
  }
}

function deriveThemeDimensionMap(
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
  const cardBorderRadius = componentOverrides?.cardBorderRadius ?? DEFAULT_DIMENSIONS.cardBorderRadius

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

function deriveThemeShadowMap(theme: ThemeName, shadowStrength: number): ThemeShadowMap {
  const shadows = theme === 'dark' ? NEUTRAL_DARK_SHADOWS : NEUTRAL_LIGHT_SHADOWS
  return {
    boxShadow: scaleShadowAlpha(shadows.boxShadow, shadowStrength),
    boxShadowSecondary: scaleShadowAlpha(shadows.boxShadowSecondary, shadowStrength),
    boxShadowTertiary: scaleShadowAlpha(shadows.boxShadowTertiary, shadowStrength),
  }
}

function deriveThemeComponentMap(
  theme: ThemeName,
  surfaces: ThemeSurfaceMap,
  primary: ThemePrimaryMap,
  error: FunctionalColorMap,
  shadowStrength: number,
): ThemeComponentMap {
  if (theme === 'dark') {
    const usesNeutralInput = surfaces.colorBgContainer === DARK_CONTAINER_BASE
    const inputOutlineColor = 'var(--cp-color-primary-bg-hover)'
    return {
      menuItemSelectedBg: surfaces.colorBgTextHover,
      inputBg: usesNeutralInput
        ? NEUTRAL_DARK_INPUT.bg
        : mix(surfaces.colorBgContainer, surfaces.colorFillSecondary, 0.375),
      inputHoverBg: usesNeutralInput
        ? NEUTRAL_DARK_INPUT.hoverBg
        : mix(surfaces.colorBgContainer, surfaces.colorFillSecondary, 0.69),
      inputActiveBg: usesNeutralInput
        ? NEUTRAL_DARK_INPUT.activeBg
        : mix(surfaces.colorBgContainer, surfaces.colorBgElevated, 0.4),
      inputErrorActiveBg: error.background,
      buttonPrimaryColor: solidControlTextColor(primary.colorPrimarySolid),
      buttonPrimaryBg: primary.colorPrimarySolid,
      buttonPrimaryHoverBg: primary.colorPrimarySolidHover,
      buttonPrimaryActiveBg: primary.colorPrimarySolidActive,
      brandMarkBg: surfaces.colorBgElevated,
      cardBg: surfaces.colorBgContainer,
      tableHeaderBg: surfaces.colorFillTertiary,
      tableRowBg: surfaces.colorBgContainer,
      tableRowStripeBg: surfaces.colorBgElevated,
      tableRowHoverBg: surfaces.colorBgTextHover,
      tableRowSelectedBg: primary.colorPrimaryBg,
      progressRemainingColor: mix(surfaces.colorBgContainer, surfaces.colorBorderSecondary, 0.92),
      layoutSiderBg: surfaces.colorBgContainer,
      cardShadow: scaleShadowAlpha(NEUTRAL_DARK_SHADOWS.cardShadow, shadowStrength),
      inputShadow: scaleShadowAlpha(NEUTRAL_DARK_SHADOWS.inputShadow, shadowStrength),
      inputHoverShadow: scaleShadowAlpha(
        `0 0 0 3px ${inputOutlineColor}, ${NEUTRAL_DARK_SHADOWS.inputHoverDropShadow}`,
        shadowStrength,
      ),
      inputActiveShadow: `0 0 0 3px ${inputOutlineColor}`,
      inputErrorActiveShadow: `0 0 0 3px ${withAlpha(error.color, 0.28)}`,
      layoutSiderShadow: scaleShadowAlpha(NEUTRAL_DARK_SHADOWS.layoutSiderShadow, shadowStrength),
      scrollbarThumbBg: mix(surfaces.colorBorderSecondary, surfaces.colorTextSecondary, 0.16),
      scrollbarThumbHoverBg: mix(surfaces.colorBorderSecondary, surfaces.colorTextSecondary, 0.3),
    }
  }

  const usesNeutralInput = surfaces.colorBgContainer === LIGHT_CONTAINER_BASE
  const inputOutlineColor = 'var(--cp-color-primary-bg-hover)'
  return {
    menuItemSelectedBg: surfaces.colorBgTextActive,
    inputBg: usesNeutralInput
      ? NEUTRAL_LIGHT_INPUT.bg
      : mix(surfaces.colorBgContainer, surfaces.colorFillSecondary, 0.78),
    inputHoverBg: usesNeutralInput
      ? NEUTRAL_LIGHT_INPUT.hoverBg
      : mix(surfaces.colorBgContainer, surfaces.colorFillTertiary, 0.88),
    inputActiveBg: usesNeutralInput ? NEUTRAL_LIGHT_INPUT.activeBg : surfaces.colorBgElevated,
    inputErrorActiveBg: error.background,
    buttonPrimaryColor: solidControlTextColor(primary.colorPrimarySolid),
    buttonPrimaryBg: primary.colorPrimarySolid,
    buttonPrimaryHoverBg: primary.colorPrimarySolidHover,
    buttonPrimaryActiveBg: primary.colorPrimarySolidActive,
    brandMarkBg: surfaces.colorBgSpotlight,
    cardBg: surfaces.colorBgContainer,
    tableHeaderBg: surfaces.colorFillTertiary,
    tableRowBg: surfaces.colorBgContainer,
    tableRowStripeBg: surfaces.colorBgElevated,
    tableRowHoverBg: surfaces.colorBgTextHover,
    tableRowSelectedBg: primary.colorPrimaryBg,
    progressRemainingColor: surfaces.colorBorderSecondary,
    layoutSiderBg: surfaces.colorBgContainer,
    cardShadow: scaleShadowAlpha(NEUTRAL_LIGHT_SHADOWS.cardShadow, shadowStrength),
    inputShadow: scaleShadowAlpha(NEUTRAL_LIGHT_SHADOWS.inputShadow, shadowStrength),
    inputHoverShadow: scaleShadowAlpha(
      `0 0 0 3px ${inputOutlineColor}, ${NEUTRAL_LIGHT_SHADOWS.inputHoverDropShadow}`,
      shadowStrength,
    ),
    inputActiveShadow: `0 0 0 3px ${inputOutlineColor}`,
    inputErrorActiveShadow: `0 0 0 3px ${withAlpha(error.color, 0.18)}`,
    layoutSiderShadow: scaleShadowAlpha(NEUTRAL_LIGHT_SHADOWS.layoutSiderShadow, shadowStrength),
    scrollbarThumbBg: mix(surfaces.colorBgLayout, surfaces.colorTextSecondary, 0.2),
    scrollbarThumbHoverBg: mix(surfaces.colorBgLayout, surfaces.colorTextSecondary, 0.34),
  }
}

function toThemeTokens(
  surfaces: ThemeSurfaceMap,
  primary: ThemePrimaryMap,
  link: ThemePrimaryMap,
  semantics: ThemeSemanticMap,
  shadows: ThemeShadowMap,
  components: ThemeComponentMap,
  dimensions: ThemeDimensionMap,
): ThemeTokens {
  return {
    '--cp-color-bg-layout': surfaces.colorBgLayout,
    '--cp-color-bg-container': surfaces.colorBgContainer,
    '--cp-color-bg-elevated': surfaces.colorBgElevated,
    '--cp-color-bg-spotlight': surfaces.colorBgSpotlight,
    '--cp-color-bg-mask': surfaces.colorBgMask,
    '--cp-color-bg-text-hover': surfaces.colorBgTextHover,
    '--cp-color-bg-text-active': surfaces.colorBgTextActive,
    '--cp-color-bg-container-disabled': surfaces.colorBgContainerDisabled,
    '--cp-color-fill-secondary': surfaces.colorFillSecondary,
    '--cp-color-fill-tertiary': surfaces.colorFillTertiary,
    '--cp-color-fill-quaternary': surfaces.colorFillQuaternary,
    '--cp-color-border': surfaces.colorBorder,
    '--cp-color-border-secondary': surfaces.colorBorderSecondary,
    '--cp-color-split': surfaces.colorSplit,
    '--cp-color-text': surfaces.colorText,
    '--cp-color-text-heading': surfaces.colorTextHeading,
    '--cp-color-text-secondary': surfaces.colorTextSecondary,
    '--cp-color-text-tertiary': surfaces.colorTextTertiary,
    '--cp-color-text-quaternary': surfaces.colorTextQuaternary,
    '--cp-color-text-disabled': surfaces.colorTextDisabled,
    '--cp-color-text-light-solid': primary.colorTextLightSolid,
    '--cp-color-shadow': surfaces.colorShadow,
    '--cp-color-primary-bg': primary.colorPrimaryBg,
    '--cp-color-primary-bg-hover': primary.colorPrimaryBgHover,
    '--cp-color-primary-border': primary.colorPrimaryBorder,
    '--cp-color-primary-border-hover': primary.colorPrimaryBorderHover,
    '--cp-color-primary-hover': primary.colorPrimaryHover,
    '--cp-color-primary': primary.colorPrimary,
    '--cp-color-primary-active': primary.colorPrimaryActive,
    '--cp-color-primary-text-hover': primary.colorPrimaryTextHover,
    '--cp-color-primary-text': primary.colorPrimaryText,
    '--cp-color-primary-text-active': primary.colorPrimaryTextActive,
    '--cp-color-link': link.colorPrimaryText,
    '--cp-color-link-hover': link.colorPrimaryTextHover,
    '--cp-color-link-active': link.colorPrimaryTextActive,
    '--cp-control-item-bg-active': primary.colorPrimaryBg,
    '--cp-control-item-bg-active-hover': primary.colorPrimaryBgHover,
    '--cp-control-outline': primary.colorPrimaryBorder,
    '--cp-menu-item-selected-bg': components.menuItemSelectedBg,
    '--cp-input-bg': components.inputBg,
    '--cp-input-hover-bg': components.inputHoverBg,
    '--cp-input-active-bg': components.inputActiveBg,
    '--cp-input-error-active-bg': components.inputErrorActiveBg,
    '--cp-button-primary-color': components.buttonPrimaryColor,
    '--cp-button-primary-bg': components.buttonPrimaryBg,
    '--cp-button-primary-hover-bg': components.buttonPrimaryHoverBg,
    '--cp-button-primary-active-bg': components.buttonPrimaryActiveBg,
    '--cp-brand-mark-bg': components.brandMarkBg,
    '--cp-card-bg': components.cardBg,
    '--cp-table-header-bg': components.tableHeaderBg,
    '--cp-table-row-bg': components.tableRowBg,
    '--cp-table-row-stripe-bg': components.tableRowStripeBg,
    '--cp-table-row-hover-bg': components.tableRowHoverBg,
    '--cp-table-row-selected-bg': components.tableRowSelectedBg,
    '--cp-progress-remaining-color': components.progressRemainingColor,
    '--cp-layout-sider-bg': components.layoutSiderBg,
    '--cp-box-shadow': shadows.boxShadow,
    '--cp-box-shadow-secondary': shadows.boxShadowSecondary,
    '--cp-box-shadow-tertiary': shadows.boxShadowTertiary,
    '--cp-card-shadow': components.cardShadow,
    '--cp-input-shadow': components.inputShadow,
    '--cp-input-hover-shadow': components.inputHoverShadow,
    '--cp-input-active-shadow': components.inputActiveShadow,
    '--cp-input-error-active-shadow': components.inputErrorActiveShadow,
    '--cp-layout-sider-shadow': components.layoutSiderShadow,
    '--cp-scrollbar-thumb-bg': components.scrollbarThumbBg,
    '--cp-scrollbar-thumb-hover-bg': components.scrollbarThumbHoverBg,
    '--cp-color-info': semantics.info.color,
    '--cp-color-info-hover': semantics.info.hover,
    '--cp-color-info-active': semantics.info.active,
    '--cp-color-info-bg': semantics.info.background,
    '--cp-color-info-border': semantics.info.border,
    '--cp-color-info-text': semantics.info.text,
    '--cp-color-success': semantics.success.color,
    '--cp-color-success-bg': semantics.success.background,
    '--cp-color-success-bg-hover': semantics.success.backgroundHover,
    '--cp-color-success-bg-active': semantics.success.backgroundActive,
    '--cp-color-success-border': semantics.success.border,
    '--cp-color-success-text': semantics.success.text,
    '--cp-color-warning': semantics.warning.color,
    '--cp-color-warning-bg': semantics.warning.background,
    '--cp-color-warning-bg-hover': semantics.warning.backgroundHover,
    '--cp-color-warning-bg-active': semantics.warning.backgroundActive,
    '--cp-color-warning-border': semantics.warning.border,
    '--cp-color-warning-text': semantics.warning.text,
    '--cp-color-error': semantics.error.color,
    '--cp-color-error-bg': semantics.error.background,
    '--cp-color-error-bg-hover': semantics.error.backgroundHover,
    '--cp-color-error-bg-active': semantics.error.backgroundActive,
    '--cp-color-error-border': semantics.error.border,
    '--cp-color-error-text': semantics.error.text,
    '--cp-font-size-xs': dimensions.fontSizeXs,
    '--cp-font-size-sm': dimensions.fontSizeSm,
    '--cp-font-size': dimensions.fontSize,
    '--cp-font-size-lg': dimensions.fontSizeLg,
    '--cp-font-size-xl': dimensions.fontSizeXl,
    '--cp-line-height-xs': dimensions.lineHeightXs,
    '--cp-line-height-sm': dimensions.lineHeightSm,
    '--cp-line-height': dimensions.lineHeight,
    '--cp-line-height-lg': dimensions.lineHeightLg,
    '--cp-line-height-xl': dimensions.lineHeightXl,
    '--cp-size-unit': dimensions.sizeUnit,
    '--cp-size-step': dimensions.sizeStep,
    '--cp-control-height-sm': dimensions.controlHeightSm,
    '--cp-control-height': dimensions.controlHeight,
    '--cp-control-height-lg': dimensions.controlHeightLg,
    '--cp-table-row-height': dimensions.tableRowHeight,
    '--cp-table-row-height-sm': dimensions.tableRowHeightSm,
    '--cp-border-radius-sm': dimensions.borderRadiusSm,
    '--cp-border-radius': dimensions.borderRadius,
    '--cp-border-radius-lg': dimensions.borderRadiusLg,
    '--cp-card-border-radius': dimensions.cardBorderRadius,
  }
}

function normalizeThemeSeedOverrides(value: unknown): ThemeSeedOverrides {
  if (!value || typeof value !== 'object')
    return {}

  const source = value as Record<string, unknown>
  const colors = [
    'colorSuccess',
    'colorWarning',
    'colorError',
    'colorInfo',
    'colorLink',
    'colorTextBase',
    'colorBgBase',
  ] as const
  const result: ThemeSeedOverrides = {}

  for (const key of colors) {
    const normalized = normalizeHexColor(source[key])
    if (normalized)
      result[key] = normalized
  }

  assignBoundedNumber(result, source, 'fontSize', 11, 17)
  assignBoundedNumber(result, source, 'sizeUnit', 3, 6)
  assignBoundedNumber(result, source, 'sizeStep', 2, 8)
  assignBoundedNumber(result, source, 'controlHeight', 28, 52)
  assignBoundedNumber(result, source, 'borderRadius', 0, 20)
  assignBoundedNumber(result, source, 'shadowStrength', 0, 140)
  return result
}

function normalizeThemeComponentOverrides(value: unknown): ThemeComponentOverrides {
  if (!value || typeof value !== 'object')
    return {}

  const source = value as Record<string, unknown>
  const result: ThemeComponentOverrides = {}
  assignBoundedNumber(result, source, 'tableRowHeight', 44, 84)
  assignBoundedNumber(result, source, 'cardBorderRadius', 0, 32)
  return result
}

function assignBoundedNumber<T extends object, K extends keyof T>(
  target: T,
  source: Record<string, unknown>,
  key: K,
  min: number,
  max: number,
) {
  const value = source[key as string]
  if (typeof value === 'number' && Number.isFinite(value))
    target[key] = Math.round(Math.min(max, Math.max(min, value))) as T[K]
}

function normalizeThemeTokenOverrides(value: unknown): Partial<ThemeTokens> {
  if (!value || typeof value !== 'object')
    return {}

  const result: Partial<ThemeTokens> = {}
  for (const [rawName, rawValue] of Object.entries(value)) {
    const name = rawName as ThemeTokenName
    if (typeof rawValue !== 'string')
      continue

    if (EDITABLE_COLOR_TOKEN_NAMES.has(name)) {
      const normalized = normalizeHexColor(rawValue)
      if (normalized)
        result[name] = normalized
      continue
    }

    if (EDITABLE_SHADOW_TOKEN_NAMES.has(name) && isSafeShadowValue(rawValue))
      result[name] = rawValue.trim()
  }
  return result
}

function isSafeShadowValue(value: string) {
  const normalized = value.trim()
  return normalized.length > 0
    && normalized.length <= 240
    && !/[;{}]/.test(normalized)
    && !/url\s*\(/i.test(normalized)
}

function scaleShadowAlpha(shadow: string, strength: number): string {
  if (strength === DEFAULT_DIMENSIONS.shadowStrength)
    return shadow
  if (strength <= 0)
    return 'none'

  const scale = strength / DEFAULT_DIMENSIONS.shadowStrength
  return shadow.replace(/#([\dA-F]{6})([\dA-F]{2})/gi, (_match, color: string, alpha: string) => {
    const scaledAlpha = Math.min(255, Math.round(Number.parseInt(alpha, 16) * scale))
      .toString(16)
      .padStart(2, '0')
      .toUpperCase()
    return `#${color.toUpperCase()}${scaledAlpha}`
  })
}

function ensureContrast(color: string, background: string, target: number): string {
  if (contrastRatio(color, background) >= target)
    return normalizeHexColor(color) ?? color

  let bestColor = color
  let bestRatio = contrastRatio(color, background)

  for (let step = 1; step <= 100; step += 1) {
    const amount = step / 100
    for (const direction of [BLACK, WHITE]) {
      const candidate = mix(color, direction, amount)
      const ratio = contrastRatio(candidate, background)
      if (ratio > bestRatio) {
        bestColor = candidate
        bestRatio = ratio
      }
      if (ratio >= target)
        return candidate
    }
  }

  return bestColor
}

/** 与 Ant Design Button 的 isBright 阈值一致；仅极亮实心色切换为深色前景。 */
function solidControlTextColor(background: string): string {
  const { red, green, blue } = hexToRgb(background)
  return red * 0.299 + green * 0.587 + blue * 0.114 > 192
    ? BLACK
    : LIGHT_FOREGROUND
}

function contrastRatio(first: string, second: string): number {
  const firstLuminance = relativeLuminance(hexToRgb(first))
  const secondLuminance = relativeLuminance(hexToRgb(second))
  const lightest = Math.max(firstLuminance, secondLuminance)
  const darkest = Math.min(firstLuminance, secondLuminance)
  return (lightest + 0.05) / (darkest + 0.05)
}

function relativeLuminance(color: RgbColor): number {
  const channels = [color.red, color.green, color.blue].map((channel) => {
    const value = channel / 255
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4
  })
  return channels[0]! * 0.2126 + channels[1]! * 0.7152 + channels[2]! * 0.0722
}

function mix(first: string, second: string, secondWeight: number): string {
  const from = hexToRgb(first)
  const to = hexToRgb(second)
  const weight = Math.min(1, Math.max(0, secondWeight))
  return rgbToHex({
    red: from.red + (to.red - from.red) * weight,
    green: from.green + (to.green - from.green) * weight,
    blue: from.blue + (to.blue - from.blue) * weight,
  })
}

function withAlpha(color: string, alpha: number): string {
  const normalized = normalizeHexColor(color) ?? DEFAULT_CUSTOM_THEME_COLOR
  const alphaHex = Math.round(Math.min(1, Math.max(0, alpha)) * 255)
    .toString(16)
    .padStart(2, '0')
    .toUpperCase()
  return `${normalized}${alphaHex}`
}

function hexToRgb(value: string): RgbColor {
  const normalized = normalizeHexColor(value) ?? DEFAULT_CUSTOM_THEME_COLOR
  return {
    red: Number.parseInt(normalized.slice(1, 3), 16),
    green: Number.parseInt(normalized.slice(3, 5), 16),
    blue: Number.parseInt(normalized.slice(5, 7), 16),
  }
}

function rgbToHex(color: RgbColor): string {
  const value = [color.red, color.green, color.blue]
    .map(channel => Math.round(Math.min(255, Math.max(0, channel))).toString(16).padStart(2, '0'))
    .join('')
  return `#${value}`.toUpperCase()
}
