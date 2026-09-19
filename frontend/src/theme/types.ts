export type ThemeMode = 'system' | 'light' | 'dark'
export type ThemeName = 'light' | 'dark'
export type ThemeColorPresetId = 'relay-blue' | 'deep-teal' | 'signal-violet' | 'graphite'
export type ThemeColorId = ThemeColorPresetId | 'custom'

export interface ThemeColorPreset {
  id: ThemeColorPresetId
  label: string
  description: string
  seed: string
  appearance?: Partial<Record<ThemeName, ThemeAppearanceSeed>>
}

/** 品牌 Seed 派生出的界面底色与正文基色。 */
export interface ThemeAppearanceSeed {
  colorBgBase: string
  colorTextBase: string
}

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

export interface RgbColor {
  red: number
  green: number
  blue: number
}

export interface ColorToneOptions {
  hueWeight?: number
  saturationScale?: number
  saturationLimit?: number
}

export type ThemePaletteStep = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10
export type ThemePaletteSource = ThemePaletteStep | 'seed'

export interface ThemeColorRoleRecipe {
  color: ThemePaletteSource
  hover: ThemePaletteSource
  active: ThemePaletteSource
  textHover: ThemePaletteSource
  text: ThemePaletteSource
  onContainer?: ThemePaletteSource
  textActive: ThemePaletteSource
  containerMix: number
  containerHoverMix: number
  containerActiveMix: number
  borderMix: number
  borderHoverMix: number
  hoverContrast?: number
  activeContrast?: number
  minimumTextLightness?: number
}

export interface ThemeColorRoleRecipes {
  primary: ThemeColorRoleRecipe
  semantic: ThemeColorRoleRecipe
  preset: ThemePresetColorRoleRecipe
}

export interface ThemePresetColorRoleRecipe {
  solid: ThemePaletteStep
  text: ThemePaletteStep
  onContainer: ThemePaletteStep
  containerMix: number
  containerStrongMix: number
  borderMix: number
  minimumTextLightness?: number
}

export interface ThemePrimaryMap {
  colorPrimaryContainer: string
  colorPrimaryContainerHover: string
  colorPrimaryBorder: string
  colorPrimaryBorderHover: string
  colorPrimaryHover: string
  colorPrimary: string
  colorPrimaryActive: string
  colorPrimaryTextHover: string
  colorPrimaryText: string
  colorPrimaryOnContainer: string
  colorPrimaryTextActive: string
  colorTextLightSolid: string
}

export interface ThemeLinkMap {
  colorLink: string
  colorLinkHover: string
  colorLinkActive: string
}

export interface ThemeSurfaceMap {
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

export interface ThemeAliasMap {
  colorFillAlter: string
}

export interface ThemeShadowMap {
  boxShadow: string
  boxShadowSecondary: string
  boxShadowTertiary: string
}

/** 中性填充的派生中间值，不直接暴露为全局 CSS Token。 */
export interface ThemeFillMap {
  controlBg: string
  controlHoverBg: string
  controlActiveBg: string
  contentBg: string
  contentStripeBg: string
  contentHoverBg: string
}

export interface ThemeComponentMap {
  menuItemSelectedBg: string
  inputBg: string
  inputHoverBg: string
  inputActiveBg: string
  inputErrorActiveBg: string
  buttonPrimaryColor: string
  buttonPrimaryBg: string
  buttonPrimaryHoverBg: string
  buttonPrimaryActiveBg: string
  buttonSecondaryBg: string
  buttonSecondaryHoverBg: string
  buttonSecondaryActiveBg: string
  iconButtonSecondaryBg: string
  iconButtonSecondaryHoverBg: string
  iconButtonSecondaryActiveBg: string
  brandMarkBg: string
  cardBg: string
  modalBg: string
  popoverHeaderBg: string
  tableHeaderBg: string
  tableRowBg: string
  tableRowStripeBg: string
  tableRowHoverBg: string
  tableRowSelectedBg: string
  tableRowSelectedHoverBg: string
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

export interface FunctionalColorMap {
  color: string
  hover: string
  active: string
  container: string
  containerHover: string
  containerActive: string
  border: string
  borderHover: string
  textHover: string
  /** 语义色在中性表面上的强调文字。 */
  text: string
  /** 语义色容器内文字与图标使用的高对比前景色。 */
  onContainer: string
  textActive: string
}

export interface ThemeSemanticMap {
  info: FunctionalColorMap
  success: FunctionalColorMap
  warning: FunctionalColorMap
  error: FunctionalColorMap
}

export interface PresetColorRoleMap {
  container: string
  containerStrong: string
  border: string
  solid: string
  text: string
  onContainer: string
}

export type ThemePresetColorName
  = 'blue'
    | 'cyan'
    | 'green'
    | 'orange'
    | 'purple'
    | 'red'

export type ThemePresetColorMap = Record<ThemePresetColorName, PresetColorRoleMap>

export interface ThemeDataMap {
  activityLevels: readonly [string, string, string, string, string]
}

export interface ThemeDimensionMap {
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

export interface ThemeMap {
  surfaces: ThemeSurfaceMap
  aliases: ThemeAliasMap
  primary: ThemePrimaryMap
  link: ThemeLinkMap
  semantics: ThemeSemanticMap
  presetColors: ThemePresetColorMap
  data: ThemeDataMap
  shadows: ThemeShadowMap
  components: ThemeComponentMap
  dimensions: ThemeDimensionMap
}

export type DirectThemeMapKey = Exclude<
  keyof ThemeMap,
  'semantics' | 'presetColors' | 'data'
>

type CamelToKebab<Value extends string> = Value extends `${infer Head}${infer Tail}`
  ? Tail extends Uncapitalize<Tail>
    ? `${Lowercase<Head>}${CamelToKebab<Tail>}`
    : `${Lowercase<Head>}-${CamelToKebab<Tail>}`
  : Value

export type DirectTokenName = {
  [Group in DirectThemeMapKey]: `--cp-${CamelToKebab<Extract<keyof ThemeMap[Group], string>>}`
}[DirectThemeMapKey]

export type FunctionalTokenSuffix
  = ''
    | '-hover'
    | '-active'
    | '-container'
    | '-container-hover'
    | '-container-active'
    | '-border'
    | '-border-hover'
    | '-text-hover'
    | '-text'
    | '-on-container'
    | '-text-active'

type SemanticColorName = Extract<keyof ThemeSemanticMap, string>
export type SemanticTokenName = `--cp-color-${SemanticColorName}${FunctionalTokenSuffix}`

export type PresetTokenSuffix
  = 'container'
    | 'container-strong'
    | 'border'
    | 'solid'
    | 'text'
    | 'on-container'

type PresetColorName = Extract<keyof ThemePresetColorMap, string>
export type PresetTokenName = `--cp-color-${PresetColorName}-${PresetTokenSuffix}`

export type ActivityLevel = 0 | 1 | 2 | 3 | 4
type ActivityTokenName = `--cp-activity-level-${ActivityLevel}`

export type ControlTokenName
  = '--cp-control-item-bg-active'
    | '--cp-control-item-bg-active-hover'
    | '--cp-control-outline'

export type ThemeTokenName
  = DirectTokenName
    | SemanticTokenName
    | PresetTokenName
    | ActivityTokenName
    | ControlTokenName

export type ThemeTokens = Record<ThemeTokenName, string>
