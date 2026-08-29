<script setup lang="ts">
import type { ThemeEditorDraft, ThemeEditorGlobalCategory } from '../composables/useThemeEditor'
import type { ResolvedTheme, ThemeColorPreset, ThemeColorPresetId, ThemeMode, ThemeSeedOverrides } from '@/theme'

import { Check, ChevronDown, Laptop, Moon, Sun } from '@lucide/vue'
import { computed } from 'vue'

import BaseSegmented from '@/components/base/BaseSegmented.vue'
import { resolveTheme, THEME_COLOR_PRESETS } from '@/theme'

import ThemeColorTokenField from './ThemeColorTokenField.vue'
import ThemeNumberTokenField from './ThemeNumberTokenField.vue'

const props = defineProps<{
  draft: ThemeEditorDraft
  resolved: ResolvedTheme
  query: string
}>()

const emit = defineEmits<{
  mode: [value: ThemeMode]
  preset: [value: ThemeColorPresetId]
  primary: [value: string]
  seedColor: [
    key: Exclude<
      keyof ThemeSeedOverrides,
      'fontSize' | 'sizeUnit' | 'sizeStep' | 'controlHeight' | 'borderRadius' | 'shadowStrength'
    >,
    value: string,
  ]
  seedNumber: [
    key: 'fontSize' | 'sizeUnit' | 'sizeStep' | 'controlHeight' | 'borderRadius' | 'shadowStrength',
    value: number,
  ]
  resetSeed: [key: keyof ThemeSeedOverrides]
}>()

const category = defineModel<ThemeEditorGlobalCategory>('category', { required: true })

const colorPresets = THEME_COLOR_PRESETS.map(preset => preset.seed)
const categoryOptions = [
  { label: '颜色', value: 'color' },
  { label: '尺寸', value: 'size' },
  { label: '风格', value: 'style' },
]
const modeOptions = [
  { label: '跟随系统', value: 'system', icon: Laptop },
  { label: '浅色', value: 'light', icon: Sun },
  { label: '深色', value: 'dark', icon: Moon },
]
const presetSwatchesById = new Map(
  THEME_COLOR_PRESETS.map(preset => [
    preset.id,
    [
      resolveTheme('light', preset.id, preset.seed).seedTokens.colorBgBase,
      preset.seed,
      resolveTheme('dark', preset.id, preset.seed).seedTokens.colorBgBase,
    ],
  ]),
)

const semanticColorFields = computed(() =>
  [
    {
      key: 'colorSuccess' as const,
      label: '成功色',
      token: 'colorSuccess',
      value: props.resolved.seedTokens.colorSuccess,
      description: '成功反馈与状态',
    },
    {
      key: 'colorWarning' as const,
      label: '警告色',
      token: 'colorWarning',
      value: props.resolved.seedTokens.colorWarning,
      description: '额度与风险提醒',
    },
    {
      key: 'colorError' as const,
      label: '错误色',
      token: 'colorError',
      value: props.resolved.seedTokens.colorError,
      description: '失败与破坏性操作',
    },
    {
      key: 'colorInfo' as const,
      label: '信息色',
      token: 'colorInfo',
      value: props.resolved.seedTokens.colorInfo,
      description: '系统与请求信息',
    },
    {
      key: 'colorLink' as const,
      label: '链接色',
      token: 'colorLink',
      value: props.resolved.seedTokens.colorLink,
      description: '链接与跳转',
    },
    {
      key: 'colorTextBase' as const,
      label: '基础文本色',
      token: 'colorTextBase',
      value: props.resolved.seedTokens.colorTextBase,
      description: '正文与信息层级',
    },
    {
      key: 'colorBgBase' as const,
      label: '基础背景色',
      token: 'colorBgBase',
      value: props.resolved.seedTokens.colorBgBase,
      description: '页面与容器基底',
    },
  ].filter(field => matches(`${field.label} ${field.token} ${field.description}`)),
)

const derivedTokens = computed(() =>
  Object.entries(props.resolved.tokens)
    .filter(([name]) => name.startsWith('--cp-color-') || name.startsWith('--cp-control-'))
    .filter(([name]) => matches(name)),
)

function matches(value: string) {
  const query = props.query.trim().toLocaleLowerCase()
  return !query || value.toLocaleLowerCase().includes(query)
}

function seedOverridden(key: keyof ThemeSeedOverrides) {
  return Object.hasOwn(props.draft.customization.seed ?? {}, key)
}

function presetSwatches(preset: ThemeColorPreset) {
  return presetSwatchesById.get(preset.id) ?? [preset.seed]
}
</script>

<template>
  <div class="grid gap-4">
    <div class="grid gap-2 rounded-cp-lg bg-cp-bg-container p-2 shadow-cp-tertiary">
      <BaseSegmented
        :model-value="draft.mode"
        label="主题模式"
        display="icon"
        :options="modeOptions"
        class="w-full"
        @update:model-value="emit('mode', $event as ThemeMode)"
      />
    </div>

    <BaseSegmented v-model="category" label="全局 Token 分类" :options="categoryOptions" class="w-full" />

    <template v-if="category === 'color'">
      <section v-if="matches('预置主题 品牌色 colorPrimary')" class="grid gap-2" aria-labelledby="theme-preset-title">
        <div class="flex items-center justify-between px-1">
          <h3 id="theme-preset-title" class="m-0 text-cp font-heavy text-cp-text">
            品牌色
          </h3>
          <code class="font-mono text-[10px] text-cp-text-quaternary">colorPrimary</code>
        </div>
        <div class="grid grid-cols-2 gap-2" role="radiogroup" aria-label="预置主题">
          <button
            v-for="preset in THEME_COLOR_PRESETS"
            :key="preset.id"
            type="button"
            role="radio"
            class="relative flex min-h-20 flex-col items-stretch justify-between gap-2 rounded-cp-lg bg-cp-fill-quaternary p-2.5 text-left outline-none transition-[background-color,box-shadow] duration-150 hover:bg-cp-bg-text-hover focus-visible:ring-2 focus-visible:ring-cp-control-outline"
            :class="draft.color === preset.id ? 'bg-cp-control-item-bg-active shadow-cp-tertiary' : undefined"
            :aria-checked="draft.color === preset.id"
            :aria-label="`${preset.label}：${preset.description}`"
            :title="preset.description"
            @click="emit('preset', preset.id)"
          >
            <span class="flex h-4 overflow-hidden rounded-full shadow-cp-tertiary">
              <span
                v-for="swatch in presetSwatches(preset)"
                :key="swatch"
                class="flex-1"
                :style="{ backgroundColor: swatch }"
              />
            </span>
            <span class="min-w-0 pr-4">
              <strong class="block text-cp-xs font-heavy text-cp-text">{{ preset.label }}</strong>
              <span class="mt-0.5 block truncate text-[9px] font-emphasis text-cp-text-quaternary">
                {{ preset.description }}
              </span>
            </span>
            <Check
              v-if="draft.color === preset.id"
              class="absolute right-2 bottom-2 size-3 text-cp-primary-text"
              stroke-width="3"
            />
          </button>
        </div>
        <ThemeColorTokenField
          label="品牌主色"
          token="colorPrimary"
          :value="resolved.seedTokens.colorPrimary"
          description="完整色阶与界面基调"
          :presets="colorPresets"
          :overridden="draft.color === 'custom'"
          @change="emit('primary', $event)"
          @reset="emit('preset', 'relay-blue')"
        />
      </section>

      <section v-if="semanticColorFields.length > 0" class="grid gap-2" aria-labelledby="theme-semantic-title">
        <h3 id="theme-semantic-title" class="m-0 px-1 text-cp font-heavy text-cp-text">
          功能色与中性色
        </h3>
        <ThemeColorTokenField
          v-for="field in semanticColorFields"
          :key="field.key"
          :label="field.label"
          :token="field.token"
          :value="field.value"
          :description="field.description"
          :presets="colorPresets"
          :overridden="seedOverridden(field.key)"
          @change="emit('seedColor', field.key, $event)"
          @reset="emit('resetSeed', field.key)"
        />
      </section>

      <details v-if="derivedTokens.length > 0" class="group rounded-cp-lg bg-cp-bg-container shadow-cp-tertiary">
        <summary
          class="flex cursor-pointer list-none items-center justify-between gap-3 px-3 py-3 text-cp-sm font-bold text-cp-text"
        >
          派生变量 Map / Alias Token
          <ChevronDown class="size-3.5 transition-transform duration-150 group-open:rotate-180" />
        </summary>
        <div class="grid gap-1 px-2 pb-2">
          <div
            v-for="[name, value] in derivedTokens"
            :key="name"
            class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 rounded-cp bg-cp-fill-quaternary px-2.5 py-2"
          >
            <code class="truncate font-mono text-[9px] text-cp-text-secondary">{{ name.replace('--cp-', '') }}</code>
            <span class="flex items-center gap-2">
              <span class="size-3 rounded-full shadow-cp-tertiary" :style="{ backgroundColor: value }" />
              <code class="font-mono text-[9px] text-cp-text-quaternary">{{ value }}</code>
            </span>
          </div>
        </div>
      </details>
    </template>

    <section v-else-if="category === 'size'" class="grid gap-2" aria-label="尺寸 Token">
      <ThemeNumberTokenField
        label="基础字号"
        token="fontSize"
        :value="resolved.seedTokens.fontSize"
        :min="11"
        :max="17"
        :overridden="seedOverridden('fontSize')"
        @change="emit('seedNumber', 'fontSize', $event)"
        @reset="emit('resetSeed', 'fontSize')"
      />
      <ThemeNumberTokenField
        label="基础间距"
        token="sizeUnit"
        :value="resolved.seedTokens.sizeUnit"
        :min="3"
        :max="6"
        :overridden="seedOverridden('sizeUnit')"
        @change="emit('seedNumber', 'sizeUnit', $event)"
        @reset="emit('resetSeed', 'sizeUnit')"
      />
      <ThemeNumberTokenField
        label="尺寸步长"
        token="sizeStep"
        :value="resolved.seedTokens.sizeStep"
        :min="2"
        :max="8"
        :overridden="seedOverridden('sizeStep')"
        @change="emit('seedNumber', 'sizeStep', $event)"
        @reset="emit('resetSeed', 'sizeStep')"
      />
      <ThemeNumberTokenField
        label="控件高度"
        token="controlHeight"
        :value="resolved.seedTokens.controlHeight"
        :min="28"
        :max="52"
        :overridden="seedOverridden('controlHeight')"
        @change="emit('seedNumber', 'controlHeight', $event)"
        @reset="emit('resetSeed', 'controlHeight')"
      />
    </section>

    <section v-else class="grid gap-2" aria-label="风格 Token">
      <div
        class="rounded-cp-lg bg-cp-primary-bg px-3 py-3 text-[10px] leading-normal font-emphasis text-cp-primary-text"
      >
        固定无边设计；圆角塑形，阴影分层。
      </div>
      <ThemeNumberTokenField
        label="通用圆角"
        token="borderRadius"
        :value="resolved.seedTokens.borderRadius"
        :min="0"
        :max="20"
        :overridden="seedOverridden('borderRadius')"
        @change="emit('seedNumber', 'borderRadius', $event)"
        @reset="emit('resetSeed', 'borderRadius')"
      />
      <ThemeNumberTokenField
        label="阴影强度"
        token="shadowStrength"
        :value="resolved.seedTokens.shadowStrength"
        :min="0"
        :max="140"
        unit="%"
        :overridden="seedOverridden('shadowStrength')"
        @change="emit('seedNumber', 'shadowStrength', $event)"
        @reset="emit('resetSeed', 'shadowStrength')"
      />
    </section>
  </div>
</template>
