<script setup lang="ts">
import type { Theme, ThemeComponentOverrides, ThemeTokenName } from '@codex-proxy/ui/theme'

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
  resolved: Theme
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
      name: 'button-primary-color',
      label: t('appearance.componentTokens.mainButtonText'),
      kind: 'color',
      description: 'primaryColor',
    },
    {
      name: 'button-primary-bg',
      label: t('appearance.componentTokens.mainButtonBackground'),
      kind: 'color',
      description: 'primaryBg',
    },
    {
      name: 'button-primary-hover-bg',
      label: t('appearance.componentTokens.mainButtonHover'),
      kind: 'color',
      description: 'primaryHoverBg',
    },
    {
      name: 'button-primary-active-bg',
      label: t('appearance.componentTokens.mainButtonActive'),
      kind: 'color',
      description: 'primaryActiveBg',
    },
    {
      name: 'button-secondary-bg',
      label: t('appearance.componentTokens.secondaryButtonBackground'),
      kind: 'color',
      description: 'secondaryBg',
    },
    {
      name: 'button-secondary-hover-bg',
      label: t('appearance.componentTokens.secondaryButtonHover'),
      kind: 'color',
      description: 'secondaryHoverBg',
    },
    {
      name: 'button-secondary-active-bg',
      label: t('appearance.componentTokens.secondaryButtonActive'),
      kind: 'color',
      description: 'secondaryActiveBg',
    },
    {
      name: 'icon-button-secondary-bg',
      label: t('appearance.componentTokens.secondaryIconButtonBackground'),
      kind: 'color',
      description: 'secondaryBg',
    },
    {
      name: 'icon-button-secondary-hover-bg',
      label: t('appearance.componentTokens.secondaryIconButtonHover'),
      kind: 'color',
      description: 'secondaryHoverBg',
    },
    {
      name: 'icon-button-secondary-active-bg',
      label: t('appearance.componentTokens.secondaryIconButtonActive'),
      kind: 'color',
      description: 'secondaryActiveBg',
    },
  ],
  form: [
    {
      name: 'input-bg',
      label: t('appearance.componentTokens.inputBoxBackground'),
      kind: 'color',
      description: 'controlBg',
    },
    {
      name: 'input-hover-bg',
      label: t('appearance.componentTokens.inputBoxHover'),
      kind: 'color',
      description: 'hoverBg',
    },
    {
      name: 'input-active-bg',
      label: t('appearance.componentTokens.inputBoxActive'),
      kind: 'color',
      description: 'activeBg',
    },
    {
      name: 'input-shadow',
      label: t('appearance.componentTokens.inputBoxShadow'),
      kind: 'shadow',
      description: 'shadow',
    },
    {
      name: 'input-hover-shadow',
      label: t('appearance.componentTokens.inputBoxHoverShadow'),
      kind: 'shadow',
      description: 'hoverShadow',
    },
    {
      name: 'input-active-shadow',
      label: t('appearance.componentTokens.inputBoxFocusOuterCircle'),
      kind: 'shadow',
      description: 'activeShadow',
    },
  ],
  surface: [
    {
      name: 'card-bg',
      label: t('appearance.componentTokens.cardBackground'),
      kind: 'color',
      description: 'colorBgContainer',
    },
    {
      name: 'modal-bg',
      label: t('appearance.componentTokens.popUpBackground'),
      kind: 'color',
      description: 'contentBg',
    },
    {
      name: 'popover-header-bg',
      label: t('appearance.componentTokens.floatingTitleBackground'),
      kind: 'color',
      description: 'headerBg',
    },
    {
      name: 'card-border-radius',
      label: t('appearance.componentTokens.cardRoundedCorners'),
      kind: 'number',
      description: 'borderRadius',
      componentKey: 'cardBorderRadius',
      min: 0,
      max: 32,
    },
    {
      name: 'card-shadow',
      label: t('appearance.componentTokens.cardShadow'),
      kind: 'shadow',
      description: 'cardShadow',
    },
  ],
  data: [
    {
      name: 'table-row-height',
      label: t('appearance.componentTokens.tableRowHeight'),
      kind: 'number',
      description: 'rowHeight',
      componentKey: 'tableRowHeight',
      min: 44,
      max: 84,
    },
    {
      name: 'table-header-bg',
      label: t('appearance.componentTokens.headerBackground'),
      kind: 'color',
      description: 'headerBg',
    },
    {
      name: 'table-row-bg',
      label: t('appearance.componentTokens.lineBackground'),
      kind: 'color',
      description: 'rowBg',
    },
    {
      name: 'table-row-stripe-bg',
      label: t('appearance.componentTokens.zebraPatternBackground'),
      kind: 'color',
      description: 'rowStripeBg',
    },
    {
      name: 'table-row-hover-bg',
      label: t('appearance.componentTokens.rowHover'),
      kind: 'color',
      description: 'rowHoverBg',
    },
    {
      name: 'table-row-selected-bg',
      label: t('appearance.componentTokens.selectedRowBackground'),
      kind: 'color',
      description: 'rowSelectedBg',
    },
    {
      name: 'table-row-selected-hover-bg',
      label: t('appearance.componentTokens.selectRowHover'),
      kind: 'color',
      description: 'rowSelectedHoverBg',
    },
    {
      name: 'progress-remaining-color',
      label: t('appearance.componentTokens.progressTrack'),
      kind: 'color',
      description: 'progressRemainingColor',
    },
  ],
  navigation: [
    {
      name: 'menu-item-selected-bg',
      label: t('appearance.componentTokens.menuSelectionBackground'),
      kind: 'color',
      description: 'itemSelectedBg',
    },
  ],
  layout: [
    {
      name: 'layout-sider-bg',
      label: t('appearance.componentTokens.sidebarBackground'),
      kind: 'color',
      description: 'siderBg',
    },
    {
      name: 'layout-sider-shadow',
      label: t('appearance.componentTokens.sidebarShadow'),
      kind: 'shadow',
      description: 'siderShadow',
    },
    {
      name: 'scrollbar-thumb-bg',
      label: t('appearance.componentTokens.scrollBarSlider'),
      kind: 'color',
      description: 'scrollbarThumbBg',
    },
    {
      name: 'scrollbar-thumb-hover-bg',
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
        <AppWindow class="size-3.5 text-z-primary-text" />
        <h3 id="component-catalog-title" class="m-0 text-z font-heavy text-z-text">
          {{ t('appearance.componentTokens.kindLabel') }}
        </h3>
      </div>
      <div class="grid grid-cols-2 gap-2" role="listbox" :aria-label="t('appearance.componentTokens.componentTokenComponentFamily')">
        <button
          v-for="option in components"
          :key="option.id"
          type="button"
          role="option"
          class="flex min-h-14 items-center gap-2.5 rounded-z-lg bg-z-fill-quaternary px-3 text-left outline-none transition-[background-color,box-shadow] duration-150 hover:bg-z-bg-text-hover focus-visible:ring-2 focus-visible:ring-z-control-outline"
          :class="component === option.id ? 'bg-z-control-item-bg-active shadow-z-tertiary' : undefined"
          :aria-selected="component === option.id"
          @click="component = option.id"
        >
          <component :is="option.icon" class="size-4 shrink-0" :class="component === option.id ? 'text-z-primary-text' : 'text-z-text-tertiary'" />
          <span class="min-w-0">
            <strong class="block truncate text-z-xs font-bold text-z-text">{{ option.label }}</strong>
            <span class="mt-0.5 block text-[9px] font-emphasis text-z-text-quaternary">{{ option.description }}</span>
          </span>
        </button>
      </div>
    </section>

    <section class="grid gap-2" :aria-label="`${activeComponent.label} Component Token`">
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

      <div v-if="visibleTokens.length === 0" class="rounded-z-lg bg-z-fill-quaternary px-3 py-6 text-center text-z-xs font-emphasis text-z-text-quaternary">
        {{ t('appearance.componentTokens.noMatchingComponentToken') }}
      </div>
    </section>
  </div>
</template>
