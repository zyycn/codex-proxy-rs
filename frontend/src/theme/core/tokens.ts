import type {
  ActivityLevel,
  ControlTokenName,
  DirectThemeMapKey,
  DirectTokenName,
  FunctionalColorMap,
  FunctionalTokenSuffix,
  PresetColorRoleMap,
  PresetTokenName,
  PresetTokenSuffix,
  SemanticTokenName,
  ThemeMap,
  ThemePresetColorMap,
  ThemePrimaryMap,
  ThemeSemanticMap,
  ThemeTokenName,
  ThemeTokens,
} from '../types'

const DIRECT_THEME_MAP_KEYS = [
  'surfaces',
  'aliases',
  'primary',
  'link',
  'components',
  'shadows',
  'dimensions',
] as const satisfies readonly DirectThemeMapKey[]

const FUNCTIONAL_TOKEN_ROLES = [
  ['background', '-bg'],
  ['backgroundHover', '-bg-hover'],
  ['backgroundActive', '-bg-active'],
  ['border', '-border'],
  ['borderHover', '-border-hover'],
  ['hover', '-hover'],
  ['color', ''],
  ['active', '-active'],
  ['textHover', '-text-hover'],
  ['text', '-text'],
  ['textActive', '-text-active'],
] as const satisfies readonly (readonly [keyof FunctionalColorMap, FunctionalTokenSuffix])[]

const PRESET_TOKEN_ROLES = [
  ['background', 'bg'],
  ['backgroundStrong', 'bg-strong'],
  ['border', 'border'],
  ['solid', 'solid'],
  ['text', 'text'],
  ['textOnBackground', 'text-on-bg'],
] as const satisfies readonly (readonly [keyof PresetColorRoleMap, PresetTokenSuffix])[]

const CONTROL_TOKEN_BINDINGS = [
  ['--cp-control-item-bg-active', 'colorPrimaryBg'],
  ['--cp-control-item-bg-active-hover', 'colorPrimaryBgHover'],
  ['--cp-control-outline', 'colorPrimaryBorder'],
] as const satisfies readonly (readonly [ControlTokenName, keyof ThemePrimaryMap])[]

export function createThemeTokens(themeMap: ThemeMap): ThemeTokens {
  const tokens: Partial<ThemeTokens> = {}

  for (const group of DIRECT_THEME_MAP_KEYS)
    appendDirectTokens(tokens, themeMap[group])

  appendPresetTokens(tokens, themeMap.presetColors)
  appendControlTokens(tokens, themeMap.primary)
  appendSemanticTokens(tokens, themeMap.semantics)
  appendActivityTokens(tokens, themeMap.data.activityLevels)

  return tokens as ThemeTokens
}

function appendDirectTokens<Source extends object>(
  target: Partial<ThemeTokens>,
  source: Source,
): void {
  for (const [key, value] of Object.entries(source))
    setToken(target, directTokenName(key), value)
}

function appendPresetTokens(
  target: Partial<ThemeTokens>,
  presetColors: ThemePresetColorMap,
): void {
  for (const [colorName, roles] of Object.entries(presetColors)) {
    for (const [role, suffix] of PRESET_TOKEN_ROLES) {
      setToken(
        target,
        `--cp-color-${colorName}-${suffix}` as PresetTokenName,
        roles[role],
      )
    }
  }
}

function appendControlTokens(
  target: Partial<ThemeTokens>,
  primary: ThemePrimaryMap,
): void {
  for (const [token, role] of CONTROL_TOKEN_BINDINGS)
    setToken(target, token, primary[role])
}

function appendSemanticTokens(
  target: Partial<ThemeTokens>,
  semantics: ThemeSemanticMap,
): void {
  for (const [colorName, roles] of Object.entries(semantics)) {
    for (const [role, suffix] of FUNCTIONAL_TOKEN_ROLES) {
      setToken(
        target,
        `--cp-color-${colorName}${suffix}` as SemanticTokenName,
        roles[role],
      )
    }
  }
}

function appendActivityTokens(
  target: Partial<ThemeTokens>,
  activityLevels: ThemeMap['data']['activityLevels'],
): void {
  activityLevels.forEach((value, level) => {
    setToken(target, `--cp-activity-level-${level as ActivityLevel}`, value)
  })
}

function directTokenName(key: string): DirectTokenName {
  const kebab = key.replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase()
  return `--cp-${kebab}` as DirectTokenName
}

function setToken(
  target: Partial<ThemeTokens>,
  name: ThemeTokenName,
  value: string,
): void {
  if (name in target)
    throw new Error(`Duplicate theme token: ${name}`)
  target[name] = value
}
