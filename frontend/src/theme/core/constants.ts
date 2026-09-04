import type {
  ThemeColorPreset,
  ThemeColorPresetId,
  ThemeCustomization,
  ThemeMode,
  ThemePresetColorName,
  ThemeTokenName,
} from '../types'
import { presetPrimaryColors } from '@ant-design/colors'

export const DEFAULT_THEME_MODE: ThemeMode = 'system'
export const DEFAULT_THEME_COLOR: ThemeColorPresetId = 'relay-blue'
export const DEFAULT_CUSTOM_THEME_COLOR = '#5983F4'
export const DEFAULT_THEME_CUSTOMIZATION: ThemeCustomization = {}

export const DEFAULT_SEED_COLORS = {
  colorSuccess: '#12B981',
  colorWarning: '#F59E0B',
  colorError: '#EF4444',
  colorInfo: DEFAULT_CUSTOM_THEME_COLOR,
} as const

export const PRESET_COLOR_SEEDS = {
  blue: presetPrimaryColors.blue,
  cyan: presetPrimaryColors.cyan,
  green: presetPrimaryColors.green,
  orange: presetPrimaryColors.orange,
  purple: presetPrimaryColors.purple,
  red: presetPrimaryColors.red,
} as const satisfies Record<ThemePresetColorName, string>

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
    seed: DEFAULT_CUSTOM_THEME_COLOR,
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

export const BLACK = '#000000'
export const WHITE = '#FFFFFF'
export const LIGHT_FOREGROUND = WHITE
export const LIGHT_CONTAINER_BASE = WHITE
export const LIGHT_TEXT_BASE = '#0E1726'
export const DARK_CONTAINER_BASE = '#111A29'
export const DARK_TEXT_BASE = '#D8E2EE'

/** 中继蓝浅色主题的稳定中性色阶；带色温主题会连续过渡到通用派生结果。 */
export const LIGHT_SURFACE_ANCHORS = {
  colorBgSpotlight: '#111827',
  colorFillSecondary: '#E9EEF5',
  colorFillTertiary: '#F1F5F9',
  colorFillQuaternary: '#F6F8FB',
  colorBorder: '#D8E0EA',
  colorBorderSecondary: '#E2E8F0',
  colorTextHeading: '#111827',
  colorTextSecondary: '#64748B',
  colorTextMuted: '#94A3B8',
} as const

/** 中继蓝暗色主题的视觉锚点；带色温主题会连续过渡到通用派生结果。 */
export const DARK_SURFACE_ANCHORS = {
  colorBgLayout: '#0B111C',
  colorBgMask: '#030712',
  colorBgSpotlight: '#050913',
} as const

/** 默认主题组件基准；带色温主题会连续过渡到 Surface 派生结果。 */
export const LIGHT_COMPONENT_ANCHORS = {
  inputHoverBg: '#F2F6FA',
  inputActiveBg: '#F8FAFC',
} as const

export const DARK_COMPONENT_ANCHORS = {
  inputActiveBg: '#111D2D',
} as const

/** 浅色阴影保持中性，不随主题文字色染色。 */
export const LIGHT_SHADOW_BASE = LIGHT_TEXT_BASE

export const EDITABLE_COLOR_TOKEN_NAMES = new Set<ThemeTokenName>([
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
  '--cp-input-shadow',
  '--cp-input-hover-shadow',
  '--cp-input-active-shadow',
  '--cp-card-shadow',
  '--cp-layout-sider-shadow',
])
