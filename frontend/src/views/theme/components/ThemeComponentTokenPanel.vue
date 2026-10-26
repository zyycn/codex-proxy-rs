<script setup lang="ts">
import type { ResolvedTheme, ThemeComponentOverrides, ThemeTokenName } from '@codex-proxy/ui/theme'

import type { Component } from 'vue'
import type {
  ThemeEditorComponent,
  ThemeEditorDraft,
} from '../composables/useThemeEditor'
import {
  AppWindow,
  Compass,
  CreditCard,
  FormInput,
  LayoutPanelLeft,
  MousePointerClick,
  TableProperties,
} from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

import ThemeColorTokenField from './ThemeColorTokenField.vue'
import ThemeNumberTokenField from './ThemeNumberTokenField.vue'
import ThemeTextTokenField from './ThemeTextTokenField.vue'

const props = defineProps<{
  draft: ThemeEditorDraft
  resolved: ResolvedTheme
  query: string
}>()

const emit = defineEmits<{
  change: [name: ThemeTokenName, value: string]
  reset: [name: ThemeTokenName]
  componentNumber: [key: keyof ThemeComponentOverrides, value: number]
  resetComponentNumber: [key: keyof ThemeComponentOverrides]
}>()

const { t } = useI18n({ useScope: 'global' })

interface ComponentOption {
  id: ThemeEditorComponent
  label: string
  description: string
  icon: Component
}

interface EditableToken {
  name: ThemeTokenName
  label: string
  kind: 'color' | 'shadow' | 'number'
  description: string
  componentKey?: keyof ThemeComponentOverrides
  min?: number
  max?: number
}

const component = defineModel<ThemeEditorComponent>({ required: true })

const components = computed<readonly ComponentOption[]>(() => ([
  {
    id: 'action',
    label: 'Action',
    description: t('appearance.preview.components.buttonsAndActions'),
    icon: MousePointerClick,
  },
  {
    id: 'form',
    label: 'Form',
    description: t('appearance.preview.components.formsAndInput'),
    icon: FormInput,
  },
  {
    id: 'surface',
    label: 'Surface',
    description: t('appearance.componentTokens.cardsAndContainers'),
    icon: CreditCard,
  },
  {
    id: 'data',
    label: 'Data Display',
    description: t('appearance.componentTokens.dataDisplay'),
    icon: TableProperties,
  },
  {
    id: 'navigation',
    label: 'Navigation',
    description: t('appearance.componentTokens.navigation'),
    icon: Compass,
  },
  {
    id: 'layout',
    label: 'Layout',
    description: t('appearance.componentTokens.layoutAndScrolling'),
    icon: LayoutPanelLeft,
  },
]))

const tokenCatalog = computed<Record<ThemeEditorComponent, readonly EditableToken[]>>(() => ({
  action: [
    {
      name: '--cp-button-primary-color',
      label: t('appearance.componentTokens.mainButtonText'),
      kind: 'color',
      description: 'primaryColor',
    },
    {
      name: '--cp-button-primary-bg',
      label: t('appearance.componentTokens.mainButtonBackground'),
      kind: 'color',
      description: 'primaryBg',
    },
    {
      name: '--cp-button-primary-hover-bg',
      label: t('appearance.componentTokens.mainButtonHover'),
      kind: 'color',
      description: 'primaryHoverBg',
    },
    {
      name: '--cp-button-primary-active-bg',
      label: t('appearance.componentTokens.mainButtonActive'),
      kind: 'color',
      description: 'primaryActiveBg',
    },
    {
      name: '--cp-button-secondary-bg',
      label: t('appearance.componentTokens.secondaryButtonBackground'),
      kind: 'color',
      description: 'secondaryBg',
    },
    {
      name: '--cp-button-secondary-hover-bg',
      label: t('appearance.componentTokens.secondaryButtonHover'),
      kind: 'color',
      description: 'secondaryHoverBg',
    },
    {
      name: '--cp-button-secondary-active-bg',
      label: t('appearance.componentTokens.secondaryButtonActive'),
      kind: 'color',
      description: 'secondaryActiveBg',
    },
    {
      name: '--cp-icon-button-secondary-bg',
      label: t('appearance.componentTokens.secondaryIconButtonBackground'),
      kind: 'color',
      description: 'secondaryBg',
    },
    {
      name: '--cp-icon-button-secondary-hover-bg',
      label: t('appearance.componentTokens.secondaryIconButtonHover'),
      kind: 'color',
      description: 'secondaryHoverBg',
    },
    {
      name: '--cp-icon-button-secondary-active-bg',
      label: t('appearance.componentTokens.secondaryIconButtonActive'),
      kind: 'color',
      description: 'secondaryActiveBg',
    },
  ],
  form: [
    {
      name: '--cp-input-bg',
      label: t('appearance.componentTokens.inputBoxBackground'),
      kind: 'color',
      description: 'controlBg',
    },
    {
      name: '--cp-input-hover-bg',
      label: t('appearance.componentTokens.inputBoxHover'),
      kind: 'color',
      description: 'hoverBg',
    },
    {
      name: '--cp-input-active-bg',
      label: t('appearance.componentTokens.inputBoxActive'),
      kind: 'color',
      description: 'activeBg',
    },
    {
      name: '--cp-input-shadow',
      label: t('appearance.componentTokens.inputBoxShadow'),
      kind: 'shadow',
      description: 'shadow',
    },
    {
      name: '--cp-input-hover-shadow',
      label: t('appearance.componentTokens.inputBoxHoverShadow'),
      kind: 'shadow',
      description: 'hoverShadow',
    },
    {
      name: '--cp-input-active-shadow',
      label: t('appearance.componentTokens.inputBoxFocusOuterCircle'),
      kind: 'shadow',
      description: 'activeShadow',
    },
  ],
  surface: [
    {
      name: '--cp-card-bg',
      label: t('appearance.componentTokens.cardBackground'),
      kind: 'color',
      description: 'colorBgContainer',
    },
    {
      name: '--cp-modal-bg',
      label: t('appearance.componentTokens.popUpBackground'),
      kind: 'color',
      description: 'contentBg',
    },
    {
      name: '--cp-popover-header-bg',
      label: t('appearance.componentTokens.floatingTitleBackground'),
      kind: 'color',
      description: 'headerBg',
    },
    {
      name: '--cp-card-border-radius',
      label: t('appearance.componentTokens.cardRoundedCorners'),
      kind: 'number',
      description: 'borderRadius',
      componentKey: 'cardBorderRadius',
      min: 0,
      max: 32,
    },
    {
      name: '--cp-card-shadow',
      label: t('appearance.componentTokens.cardShadow'),
      kind: 'shadow',
      description: 'cardShadow',
    },
  ],
  data: [
    {
      name: '--cp-table-row-height',
      label: t('appearance.componentTokens.tableRowHeight'),
      kind: 'number',
      description: 'rowHeight',
      componentKey: 'tableRowHeight',
      min: 44,
      max: 84,
    },
    {
      name: '--cp-table-header-bg',
      label: t('appearance.componentTokens.headerBackground'),
      kind: 'color',
      description: 'headerBg',
    },
    {
      name: '--cp-table-row-bg',
      label: t('appearance.componentTokens.lineBackground'),
      kind: 'color',
      description: 'rowBg',
    },
    {
      name: '--cp-table-row-stripe-bg',
      label: t('appearance.componentTokens.zebraPatternBackground'),
      kind: 'color',
      description: 'rowStripeBg',
    },
    {
      name: '--cp-table-row-hover-bg',
      label: t('appearance.componentTokens.rowHover'),
      kind: 'color',
      description: 'rowHoverBg',
    },
    {
      name: '--cp-table-row-selected-bg',
      label: t('appearance.componentTokens.selectedRowBackground'),
      kind: 'color',
      description: 'rowSelectedBg',
    },
    {
      name: '--cp-table-row-selected-hover-bg',
      label: t('appearance.componentTokens.selectRowHover'),
      kind: 'color',
      description: 'rowSelectedHoverBg',
    },
    {
      name: '--cp-progress-remaining-color',
      label: t('appearance.componentTokens.progressTrack'),
      kind: 'color',
      description: 'progressRemainingColor',
    },
  ],
  navigation: [
    {
      name: '--cp-menu-item-selected-bg',
      label: t('appearance.componentTokens.menuSelectionBackground'),
      kind: 'color',
      description: 'itemSelectedBg',
    },
  ],
  layout: [
    {
      name: '--cp-layout-sider-bg',
      label: t('appearance.componentTokens.sidebarBackground'),
      kind: 'color',
      description: 'siderBg',
    },
    {
      name: '--cp-layout-sider-shadow',
      label: t('appearance.componentTokens.sidebarShadow'),
      kind: 'shadow',
      description: 'siderShadow',
    },
    {
      name: '--cp-scrollbar-thumb-bg',
      label: t('appearance.componentTokens.scrollBarSlider'),
      kind: 'color',
      description: 'scrollbarThumbBg',
    },
    {
      name: '--cp-scrollbar-thumb-hover-bg',
      label: t('appearance.componentTokens.scrollBarHover'),
      kind: 'color',
      description: 'scrollbarThumbHoverBg',
    },
  ],
}))

const activeComponent = computed(() =>
  components.value.find(option => option.id === component.value) ?? components.value[0]!,
)
const visibleTokens = computed(() => tokenCatalog.value[component.value].filter((token) => {
  const query = props.query.trim().toLocaleLowerCase()
  return !query || `${token.label} ${token.name} ${token.description}`.toLocaleLowerCase().includes(query)
}))

function overridden(name: ThemeTokenName) {
  return Object.hasOwn(props.draft.customization.tokenOverrides ?? {}, name)
}

function componentOverridden(key: keyof ThemeComponentOverrides | undefined) {
  return key
    ? Object.hasOwn(props.draft.customization.component ?? {}, key)
    : false
}

function numericTokenValue(name: ThemeTokenName) {
  return Number.parseFloat(props.resolved.tokens[name])
}
</script>

<template>
  <div class="grid gap-4">
    <section aria-labelledby="component-catalog-title">
      <div class="mb-2 flex items-center gap-2 px-1">
        <AppWindow class="size-3.5 text-cp-primary-text" />
        <h3 id="component-catalog-title" class="m-0 text-cp font-heavy text-cp-text">
          {{ t('appearance.componentTokens.kindLabel') }}
        </h3>
      </div>
      <div class="grid grid-cols-2 gap-2" role="listbox" :aria-label="t('appearance.componentTokens.componentTokenComponentFamily')">
        <button
          v-for="option in components"
          :key="option.id"
          type="button"
          role="option"
          class="flex min-h-14 items-center gap-2.5 rounded-cp-lg bg-cp-fill-quaternary px-3 text-left outline-none transition-[background-color,box-shadow] duration-150 hover:bg-cp-bg-text-hover focus-visible:ring-2 focus-visible:ring-cp-control-outline"
          :class="component === option.id ? 'bg-cp-control-item-bg-active shadow-cp-tertiary' : undefined"
          :aria-selected="component === option.id"
          @click="component = option.id"
        >
          <component :is="option.icon" class="size-4 shrink-0" :class="component === option.id ? 'text-cp-primary-text' : 'text-cp-text-tertiary'" />
          <span class="min-w-0">
            <strong class="block truncate text-cp-xs font-bold text-cp-text">{{ option.label }}</strong>
            <span class="mt-0.5 block text-[9px] font-emphasis text-cp-text-quaternary">{{ option.description }}</span>
          </span>
        </button>
      </div>
    </section>

    <section class="grid gap-2" :aria-label="`${activeComponent.label} Component Token`">
      <div class="rounded-cp-lg bg-cp-bg-container px-3 py-3 shadow-cp-tertiary">
        <p class="m-0 text-[10px] leading-normal font-emphasis text-cp-text-secondary">
          {{ t('appearance.componentTokens.description', { label: activeComponent.label }) }}
        </p>
      </div>

      <template v-for="token in visibleTokens" :key="token.name">
        <ThemeColorTokenField
          v-if="token.kind === 'color'"
          :label="token.label"
          :token="token.description"
          :value="resolved.tokens[token.name]"
          :overridden="overridden(token.name)"
          @change="emit('change', token.name, $event)"
          @reset="emit('reset', token.name)"
        />
        <ThemeTextTokenField
          v-else-if="token.kind === 'shadow'"
          :label="token.label"
          :token="token.description"
          :value="resolved.tokens[token.name]"
          :overridden="overridden(token.name)"
          @change="emit('change', token.name, $event)"
          @reset="emit('reset', token.name)"
        />
        <ThemeNumberTokenField
          v-else-if="token.componentKey"
          :label="token.label"
          :token="token.description"
          :value="numericTokenValue(token.name)"
          :min="token.min ?? 0"
          :max="token.max ?? 100"
          :overridden="componentOverridden(token.componentKey)"
          @change="emit('componentNumber', token.componentKey, $event)"
          @reset="emit('resetComponentNumber', token.componentKey)"
        />
      </template>

      <div v-if="visibleTokens.length === 0" class="rounded-cp-lg bg-cp-fill-quaternary px-3 py-6 text-center text-cp-xs font-emphasis text-cp-text-quaternary">
        {{ t('appearance.componentTokens.noMatchingComponentToken') }}
      </div>
    </section>
  </div>
</template>
