<script setup lang="ts">
import type { ResolvedTheme, ThemeColorPreset, ThemeColorPresetId, ThemeMode, ThemeSeedOverrides } from '@codex-proxy/ui/theme'

import type { ThemeEditorDraft, ThemeEditorGlobalCategory } from '../composables/useThemeEditor'
import { ZSegmented } from '@codex-proxy/ui'

import { resolveThemeSeedTokens, THEME_COLOR_PRESETS } from '@codex-proxy/ui/theme'
import { Check, ChevronDown, Laptop, Moon, Sun } from '@lucide/vue'

import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { uiLocale } from '@/i18n/ui'

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

const { t } = useI18n({ useScope: 'global' })

const category = defineModel<ThemeEditorGlobalCategory>('category', { required: true })
const derivedOpen = shallowRef(false)

const localizedPresets = computed(() => THEME_COLOR_PRESETS.map(preset => ({
  ...preset,
  ...uiLocale.value.theme[preset.id],
})))
const colorPresets = THEME_COLOR_PRESETS.map(preset => preset.seed)
const categoryOptions = computed(() => ([
  {
    label: t('common.fields.color'),
    value: 'color',
  },
  {
    label: t('appearance.globalTokens.size'),
    value: 'size',
  },
  {
    label: t('appearance.globalTokens.style'),
    value: 'style',
  },
]))
const modeOptions = computed(() => ([
  {
    label: t('appearance.globalTokens.systemMode'),
    value: 'system',
    icon: Laptop,
  },
  {
    label: t('appearance.globalTokens.lightColor'),
    value: 'light',
    icon: Sun,
  },
  {
    label: t('appearance.globalTokens.dark'),
    value: 'dark',
    icon: Moon,
  },
]))
const presetSwatchesById = new Map(
  THEME_COLOR_PRESETS.map(preset => [
    preset.id,
    [
      resolveThemeSeedTokens('light', preset.seed, undefined, preset.appearance?.light).colorBgBase,
      preset.seed,
      resolveThemeSeedTokens('dark', preset.seed, undefined, preset.appearance?.dark).colorBgBase,
    ],
  ]),
)

const semanticColorFields = computed(() =>
  [
    {
      key: 'colorSuccess' as const,
      label: t('appearance.globalTokens.successColor'),
      token: 'colorSuccess',
      value: props.resolved.seedTokens.colorSuccess,
      description: t('appearance.globalTokens.successFeedbackAndStatus'),
    },
    {
      key: 'colorWarning' as const,
      label: t('appearance.globalTokens.warningColor'),
      token: 'colorWarning',
      value: props.resolved.seedTokens.colorWarning,
      description: t('appearance.globalTokens.limitAndRiskReminder'),
    },
    {
      key: 'colorError' as const,
      label: t('appearance.globalTokens.wrongColor'),
      token: 'colorError',
      value: props.resolved.seedTokens.colorError,
      description: t('appearance.globalTokens.dangerHelp'),
    },
    {
      key: 'colorInfo' as const,
      label: t('appearance.globalTokens.informationColor'),
      token: 'colorInfo',
      value: props.resolved.seedTokens.colorInfo,
      description: t('appearance.globalTokens.systemAndRequestInformation'),
    },
    {
      key: 'colorLink' as const,
      label: t('appearance.globalTokens.linkColor'),
      token: 'colorLink',
      value: props.resolved.seedTokens.colorLink,
      description: t('appearance.globalTokens.linksAndJumps'),
    },
    {
      key: 'colorTextBase' as const,
      label: t('appearance.globalTokens.basicTextColor'),
      token: 'colorTextBase',
      value: props.resolved.seedTokens.colorTextBase,
      description: t('appearance.globalTokens.textAndInformationLevels'),
    },
    {
      key: 'colorBgBase' as const,
      label: t('appearance.globalTokens.basicBackgroundColor'),
      token: 'colorBgBase',
      value: props.resolved.seedTokens.colorBgBase,
      description: t('appearance.globalTokens.pageAndContainerBase'),
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
      <ZSegmented
        :model-value="draft.mode"
        :aria-label="t('appearance.globalTokens.themeMode')"
        display="icon"
        :options="modeOptions"
        class="w-full"
        @update:model-value="emit('mode', $event as ThemeMode)"
      />
    </div>

    <ZSegmented v-model="category" :aria-label="t('appearance.globalTokens.globalTokenClassification')" :options="categoryOptions" class="w-full" />

    <template v-if="category === 'color'">
      <section v-if="matches(t('appearance.globalTokens.brandColorLabel'))" class="grid gap-2" aria-labelledby="theme-preset-title">
        <div class="flex items-center justify-between px-1">
          <h3 id="theme-preset-title" class="m-0 text-cp font-heavy text-cp-text">
            {{ t('appearance.globalTokens.brandColor') }}
          </h3>
          <code class="font-mono text-[10px] text-cp-text-quaternary">colorPrimary</code>
        </div>
        <div class="grid grid-cols-2 gap-2" role="radiogroup" :aria-label="t('appearance.globalTokens.presetThemes')">
          <button
            v-for="preset in localizedPresets"
            :key="preset.id"
            type="button"
            role="radio"
            class="relative flex min-h-20 flex-col items-stretch justify-between gap-2 rounded-cp-lg bg-cp-fill-quaternary p-2.5 text-left outline-none transition-[background-color,box-shadow] duration-150 hover:bg-cp-bg-text-hover focus-visible:ring-2 focus-visible:ring-cp-control-outline"
            :class="draft.color === preset.id ? 'bg-cp-control-item-bg-active shadow-cp-tertiary' : undefined"
            :aria-checked="draft.color === preset.id"
            :aria-label="`${preset.label}: ${preset.description}`"
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
          :label="t('appearance.globalTokens.brandMainColor')"
          token="colorPrimary"
          :value="resolved.seedTokens.colorPrimary"
          :description="t('appearance.globalTokens.paletteDescription')"
          :presets="colorPresets"
          :overridden="draft.color === 'custom'"
          @change="emit('primary', $event)"
          @reset="emit('preset', 'relay-blue')"
        />
      </section>

      <section v-if="semanticColorFields.length > 0" class="grid gap-2" aria-labelledby="theme-semantic-title">
        <h3 id="theme-semantic-title" class="m-0 px-1 text-cp font-heavy text-cp-text">
          {{ t('appearance.globalTokens.semanticColorsTitle') }}
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

      <details
        v-if="derivedTokens.length > 0"
        :open="derivedOpen"
        class="group rounded-cp-lg bg-cp-bg-container shadow-cp-tertiary"
        @toggle="derivedOpen = ($event.target as HTMLDetailsElement).open"
      >
        <summary
          class="flex cursor-pointer list-none items-center justify-between gap-3 px-3 py-3 text-cp-sm font-bold text-cp-text"
        >
          {{ t('appearance.globalTokens.derivedVariablesMapAliasToken') }}
          <ChevronDown class="size-3.5 transition-transform duration-150 group-open:rotate-180" />
        </summary>
        <div v-if="derivedOpen" class="grid gap-1 px-2 pb-2">
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

    <section v-else-if="category === 'size'" class="grid gap-2" :aria-label="t('appearance.globalTokens.sizeToken')">
      <ThemeNumberTokenField
        :label="t('appearance.globalTokens.basicFontSize')"
        token="fontSize"
        :value="resolved.seedTokens.fontSize"
        :min="11"
        :max="17"
        :overridden="seedOverridden('fontSize')"
        @change="emit('seedNumber', 'fontSize', $event)"
        @reset="emit('resetSeed', 'fontSize')"
      />
      <ThemeNumberTokenField
        :label="t('appearance.globalTokens.basicSpacing')"
        token="sizeUnit"
        :value="resolved.seedTokens.sizeUnit"
        :min="3"
        :max="6"
        :overridden="seedOverridden('sizeUnit')"
        @change="emit('seedNumber', 'sizeUnit', $event)"
        @reset="emit('resetSeed', 'sizeUnit')"
      />
      <ThemeNumberTokenField
        :label="t('appearance.globalTokens.sizeStep')"
        token="sizeStep"
        :value="resolved.seedTokens.sizeStep"
        :min="2"
        :max="8"
        :overridden="seedOverridden('sizeStep')"
        @change="emit('seedNumber', 'sizeStep', $event)"
        @reset="emit('resetSeed', 'sizeStep')"
      />
      <ThemeNumberTokenField
        :label="t('appearance.preview.components.controlHeight')"
        token="controlHeight"
        :value="resolved.seedTokens.controlHeight"
        :min="28"
        :max="52"
        :overridden="seedOverridden('controlHeight')"
        @change="emit('seedNumber', 'controlHeight', $event)"
        @reset="emit('resetSeed', 'controlHeight')"
      />
    </section>

    <section v-else class="grid gap-2" :aria-label="t('appearance.globalTokens.styleToken')">
      <div
        class="rounded-cp-lg bg-cp-primary-container px-3 py-3 text-[10px] leading-normal font-emphasis text-cp-primary-on-container"
      >
        {{ t('appearance.globalTokens.surfaceDescription') }}
      </div>
      <ThemeNumberTokenField
        :label="t('appearance.globalTokens.universalFillet')"
        token="borderRadius"
        :value="resolved.seedTokens.borderRadius"
        :min="0"
        :max="20"
        :overridden="seedOverridden('borderRadius')"
        @change="emit('seedNumber', 'borderRadius', $event)"
        @reset="emit('resetSeed', 'borderRadius')"
      />
      <ThemeNumberTokenField
        :label="t('appearance.globalTokens.shadowIntensity')"
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
