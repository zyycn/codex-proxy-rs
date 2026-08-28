import type {
  ThemeColorPreset,
  ThemeColorPresetId,
  ThemeCustomization,
  ThemeMode,
  ThemeTokenName,
} from './index'

export const DEFAULT_THEME_MODE: ThemeMode = 'system'
export const DEFAULT_THEME_COLOR: ThemeColorPresetId = 'relay-blue'
export const DEFAULT_CUSTOM_THEME_COLOR = '#5983F4'
export const DEFAULT_THEME_CUSTOMIZATION: ThemeCustomization = {}

export const DEFAULT_SEED_COLORS = {
  colorSuccess: '#12B981',
  colorWarning: '#F59E0B',
  colorError: '#EF4444',
  colorInfo: '#5983F4',
} as const

export const DEFAULT_DIMENSIONS = {
  fontSize: 13,
  sizeUnit: 4,
  sizeStep: 4,
  controlHeight: 38,
  tableRowHeight: 60,
  borderRadius: 8,
  cardBorderRadius: 18,
  shadowStrength: 100,
} as const

export const THEME_COLOR_PRESETS: readonly ThemeColorPreset[] = [
  {
    id: 'relay-blue',
    label: '中继蓝',
    description: '石墨表面上的清晰交互信号',
    seed: '#5983F4',
  },
  {
    id: 'deep-teal',
    label: '深海青',
    description: '氧化铜落入深水玻璃的冷静层次',
    seed: '#0E7C72',
    appearance: {
      light: {
        colorBgBase: '#FBFEFD',
        colorTextBase: '#122C2A',
      },
      dark: {
        colorBgBase: '#102321',
        colorTextBase: '#D7EAE6',
      },
    },
  },
  {
    id: 'signal-violet',
    label: '古风色',
    description: '赭石落在宣纸与松烟墨上的温厚层次',
    seed: '#A0583D',
    appearance: {
      light: {
        colorBgBase: '#F9F3E8',
        colorTextBase: '#2B211B',
      },
      dark: {
        colorBgBase: '#1E1713',
        colorTextBase: '#EEE3D3',
      },
    },
  },
  {
    id: 'graphite',
    label: '石墨',
    description: '铅笔粉末与工作室灰纸的材料质感',
    seed: '#525B66',
    appearance: {
      light: {
        colorBgBase: '#FAF9F6',
        colorTextBase: '#25272B',
      },
      dark: {
        colorBgBase: '#1B1C1E',
        colorTextBase: '#E5E1D9',
      },
    },
  },
] as const

export const LIGHT_FOREGROUND = '#FFFFFF'
export const LIGHT_CONTAINER_BASE = '#FFFFFF'
export const LIGHT_TEXT_BASE = '#0E1726'
export const DARK_CONTAINER_BASE = '#111A29'
export const DARK_SPOTLIGHT_BASE = '#030710'
export const DARK_TEXT_BASE = '#D8E2EE'
export const BLACK = '#000000'
export const WHITE = '#FFFFFF'

export const NEUTRAL_LIGHT_SURFACES = {
  colorBgLayout: '#F6F8FB',
  colorBgContainer: LIGHT_CONTAINER_BASE,
  colorBgElevated: LIGHT_CONTAINER_BASE,
  colorBgSpotlight: '#111827',
  colorBgMask: '#0E17264D',
  colorBgTextHover: '#F1F5F9',
  colorBgTextActive: '#E9EEF5',
  colorBgContainerDisabled: '#F1F5F9',
  colorFillSecondary: '#E9EEF5',
  colorFillTertiary: '#F1F5F9',
  colorFillQuaternary: '#F6F8FB',
  colorBorder: '#D8E0EA',
  colorBorderSecondary: '#E2E8F0',
  colorSplit: '#D8E0EA6B',
  colorTextHeading: '#111827',
  colorTextSecondary: '#64748B',
  colorTextTertiary: '#94A3B8',
  colorTextQuaternary: '#94A3B8',
  colorTextDisabled: '#94A3B8',
} as const

export const NEUTRAL_DARK_SURFACES = {
  colorBgLayout: '#0B111C',
  colorBgContainer: DARK_CONTAINER_BASE,
  colorBgElevated: '#162133',
  colorBgSpotlight: '#050913',
  colorBgMask: '#030712A6',
  colorBgTextHover: '#1B2A40',
  colorBgTextActive: '#23354F',
  colorBgContainerDisabled: '#182231',
  colorFillSecondary: '#23354F',
  colorFillTertiary: '#1C2A3D',
  colorFillQuaternary: '#162133',
  colorBorder: '#29384D',
  colorBorderSecondary: '#26364C',
  colorSplit: '#26364C80',
  colorTextHeading: '#F5F8FC',
  colorTextSecondary: '#8FA0B7',
  colorTextTertiary: '#71839C',
  colorTextQuaternary: '#64748B',
  colorTextDisabled: '#53647A',
} as const

export const NEUTRAL_LIGHT_INPUT = {
  bg: '#EEF2F7',
  hoverBg: '#F2F6FA',
  activeBg: '#F8FAFC',
} as const

export const NEUTRAL_DARK_INPUT = {
  bg: '#182437',
  hoverBg: '#1D2D43',
  activeBg: '#111D2D',
} as const

export const NEUTRAL_DARK_SEMANTIC_TEXT = {
  info: '#93C5FD',
  success: '#86EFAC',
  warning: '#FCD34D',
  error: '#FDA4AF',
} as const

/** 阴影保持中性，不跟随主题主色染色。 */
export const NEUTRAL_LIGHT_SHADOWS = {
  boxShadow: '0 18px 38px -18px #0E17262B',
  boxShadowSecondary: '0 10px 22px -18px #0E172614',
  boxShadowTertiary: '0 9px 18px -14px #0E172616',
  cardShadow: '0 10px 22px -18px #0E172614',
  inputShadow: '0 9px 18px -14px #0E172616',
  inputHoverDropShadow: '0 12px 24px -16px #0E17261F',
  layoutSiderShadow: '2px 0 12px -12px #0E172607',
} as const

export const NEUTRAL_DARK_SHADOWS = {
  boxShadow: '0 24px 52px -26px #000000D1',
  boxShadowSecondary: '0 18px 34px -26px #000000A3',
  boxShadowTertiary: '0 12px 24px -20px #000000A8',
  cardShadow: '0 18px 34px -26px #000000A3',
  inputShadow: '0 12px 24px -20px #000000A8',
  inputHoverDropShadow: '0 14px 28px -22px #000000B8',
  layoutSiderShadow: '2px 0 18px -14px #000000B8',
} as const

export const EDITABLE_COLOR_TOKEN_NAMES = new Set<ThemeTokenName>([
  '--cp-color-bg-elevated',
  '--cp-color-bg-text-hover',
  '--cp-color-bg-text-active',
  '--cp-control-item-bg-active',
  '--cp-control-item-bg-active-hover',
  '--cp-menu-item-selected-bg',
  '--cp-input-bg',
  '--cp-input-hover-bg',
  '--cp-input-active-bg',
  '--cp-button-primary-color',
  '--cp-button-primary-bg',
  '--cp-button-primary-hover-bg',
  '--cp-button-primary-active-bg',
  '--cp-card-bg',
  '--cp-modal-bg',
  '--cp-table-header-bg',
  '--cp-table-row-bg',
  '--cp-table-row-stripe-bg',
  '--cp-table-row-hover-bg',
  '--cp-table-row-selected-bg',
  '--cp-progress-remaining-color',
  '--cp-layout-sider-bg',
  '--cp-scrollbar-thumb-bg',
  '--cp-scrollbar-thumb-hover-bg',
])

export const EDITABLE_SHADOW_TOKEN_NAMES = new Set<ThemeTokenName>([
  '--cp-box-shadow',
  '--cp-input-shadow',
  '--cp-input-hover-shadow',
  '--cp-input-active-shadow',
  '--cp-card-shadow',
  '--cp-layout-sider-shadow',
])
