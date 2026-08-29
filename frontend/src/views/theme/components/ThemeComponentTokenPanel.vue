<script setup lang="ts">
import type { Component } from 'vue'
import type {
  ThemeEditorComponent,
  ThemeEditorDraft,
} from '../composables/useThemeEditor'
import type { ResolvedTheme, ThemeComponentOverrides, ThemeTokenName } from '@/theme'

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

import ThemeColorTokenField from './ThemeColorTokenField.vue'
import ThemeNumberTokenField from './ThemeNumberTokenField.vue'
import ThemeTextTokenField from './ThemeTextTokenField.vue'

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

const component = defineModel<ThemeEditorComponent>({ required: true })

const components: readonly ComponentOption[] = [
  { id: 'action', label: 'Action', description: '按钮与动作', icon: MousePointerClick },
  { id: 'form', label: 'Form', description: '表单与输入', icon: FormInput },
  { id: 'surface', label: 'Surface', description: '卡片与容器', icon: CreditCard },
  { id: 'data', label: 'Data Display', description: '数据展示', icon: TableProperties },
  { id: 'navigation', label: 'Navigation', description: '导航', icon: Compass },
  { id: 'layout', label: 'Layout', description: '布局与滚动', icon: LayoutPanelLeft },
]

const tokenCatalog: Record<ThemeEditorComponent, readonly EditableToken[]> = {
  action: [
    { name: '--cp-button-primary-color', label: '主按钮文字', kind: 'color', description: 'primaryColor' },
    { name: '--cp-button-primary-bg', label: '主按钮背景', kind: 'color', description: 'primaryBg' },
    { name: '--cp-button-primary-hover-bg', label: '主按钮 Hover', kind: 'color', description: 'primaryHoverBg' },
    { name: '--cp-button-primary-active-bg', label: '主按钮 Active', kind: 'color', description: 'primaryActiveBg' },
  ],
  form: [
    { name: '--cp-input-bg', label: '输入框背景', kind: 'color', description: 'colorBgContainer' },
    { name: '--cp-input-hover-bg', label: '输入框 Hover', kind: 'color', description: 'hoverBg' },
    { name: '--cp-input-active-bg', label: '输入框 Active', kind: 'color', description: 'activeBg' },
    { name: '--cp-input-shadow', label: '输入框阴影', kind: 'shadow', description: 'shadow' },
    { name: '--cp-input-hover-shadow', label: '输入框 Hover 阴影', kind: 'shadow', description: 'hoverShadow' },
    { name: '--cp-input-active-shadow', label: '输入框焦点阴影', kind: 'shadow', description: 'activeShadow' },
  ],
  surface: [
    { name: '--cp-card-bg', label: '卡片背景', kind: 'color', description: 'colorBgContainer' },
    { name: '--cp-modal-bg', label: '弹窗背景', kind: 'color', description: 'contentBg' },
    {
      name: '--cp-card-border-radius',
      label: '卡片圆角',
      kind: 'number',
      description: 'borderRadius',
      componentKey: 'cardBorderRadius',
      min: 0,
      max: 32,
    },
    { name: '--cp-card-shadow', label: '卡片阴影', kind: 'shadow', description: 'cardShadow' },
  ],
  data: [
    {
      name: '--cp-table-row-height',
      label: '表格行高',
      kind: 'number',
      description: 'rowHeight',
      componentKey: 'tableRowHeight',
      min: 44,
      max: 84,
    },
    { name: '--cp-table-header-bg', label: '表头背景', kind: 'color', description: 'headerBg' },
    { name: '--cp-table-row-bg', label: '行背景', kind: 'color', description: 'rowBg' },
    { name: '--cp-table-row-stripe-bg', label: '斑马纹背景', kind: 'color', description: 'rowStripeBg' },
    { name: '--cp-table-row-hover-bg', label: '行 Hover', kind: 'color', description: 'rowHoverBg' },
    { name: '--cp-table-row-selected-bg', label: '选中行背景', kind: 'color', description: 'rowSelectedBg' },
    { name: '--cp-progress-remaining-color', label: '进度轨道', kind: 'color', description: 'progressRemainingColor' },
  ],
  navigation: [
    { name: '--cp-menu-item-selected-bg', label: '菜单选中背景', kind: 'color', description: 'itemSelectedBg' },
  ],
  layout: [
    { name: '--cp-layout-sider-bg', label: '侧栏背景', kind: 'color', description: 'siderBg' },
    { name: '--cp-layout-sider-shadow', label: '侧栏阴影', kind: 'shadow', description: 'siderShadow' },
    { name: '--cp-scrollbar-thumb-bg', label: '滚动条滑块', kind: 'color', description: 'scrollbarThumbBg' },
    { name: '--cp-scrollbar-thumb-hover-bg', label: '滚动条 Hover', kind: 'color', description: 'scrollbarThumbHoverBg' },
  ],
}

const activeComponent = computed(() =>
  components.find(option => option.id === component.value) ?? components[0]!,
)
const visibleTokens = computed(() => tokenCatalog[component.value].filter((token) => {
  const query = props.query.trim().toLocaleLowerCase()
  return !query || `${token.label} ${token.name} ${token.description}`.toLocaleLowerCase().includes(query)
}))

function overridden(name: ThemeTokenName) {
  return Object.hasOwn(props.draft.customization.tokenOverrides ?? {}, name)
}

function componentOverridden(key: keyof ThemeComponentOverrides | undefined) {
  return key ? Object.hasOwn(props.draft.customization.component ?? {}, key) : false
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
          Component Token
        </h3>
      </div>
      <div class="grid grid-cols-2 gap-2" role="listbox" aria-label="Component Token 组件族">
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
          调整 {{ activeComponent.label }} 的 Component Token；未修改的值继续由全局 Seed / Alias 算法生成。
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
        没有匹配的 Component Token
      </div>
    </section>
  </div>
</template>
