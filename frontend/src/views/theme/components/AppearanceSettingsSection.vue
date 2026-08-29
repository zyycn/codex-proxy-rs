<script setup lang="ts">
import { Check, RotateCcw, Save, Search, Undo2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import { toast } from '@/components/base/BaseToast'

import { useThemeEditor } from '../composables/useThemeEditor'
import ThemeComponentTokenPanel from './ThemeComponentTokenPanel.vue'
import ThemeEditorPreview from './ThemeEditorPreview.vue'
import ThemeGlobalTokenPanel from './ThemeGlobalTokenPanel.vue'

const {
  draft,
  scope,
  globalCategory,
  preview,
  previewTheme,
  component,
  query,
  resolvedPreview,
  previewStyle,
  modificationCount,
  dirty,
  selectPreset,
  setPrimaryColor,
  setMode,
  setSeedColor,
  setSeedNumber,
  resetSeed,
  setComponentNumber,
  resetComponentNumber,
  setTokenOverride,
  resetTokenOverride,
  resetDraft,
  restoreDefaults,
  save,
} = useThemeEditor()

const scopeOptions = [
  { label: '全局', value: 'global' },
  { label: '组件', value: 'component' },
]

function switchScope(value: string) {
  scope.value = value === 'component' ? 'component' : 'global'
}

function saveTheme(event: MouseEvent) {
  if (!save(event)) {
    toast.error('主题设置无效，请检查编辑值')
    return
  }
  toast.success('主题已保存并应用')
}
</script>

<template>
  <div class="flex min-h-0 w-full flex-col">
    <BasePageHeader class="xl:h-17" title="主题设置">
      <template #description>
        <span class="flex flex-wrap items-center gap-x-2 gap-y-1">
          <span>自定义当前浏览器中的管理端配色与界面风格</span>
          <span
            class="inline-flex items-center gap-1.5 text-cp-sm font-heavy"
            :class="dirty ? 'text-cp-primary-text' : 'text-cp-text-tertiary'"
          >
            <span class="size-1.5 rounded-full bg-current" />
            {{ dirty ? `${modificationCount} 处修改待应用` : '已同步' }}
          </span>
        </span>
      </template>

      <template #actions>
        <div class="flex flex-wrap items-center justify-end gap-2">
          <BaseSegmented
            :model-value="scope"
            label="主题编辑层级"
            :options="scopeOptions"
            size="sm"
            @update:model-value="switchScope"
          />
          <BaseButton size="sm" variant="ghost" :disabled="!dirty" @click="resetDraft">
            <template #icon>
              <Undo2 class="size-3.5" />
            </template>
            撤销草稿
          </BaseButton>
          <BaseButton size="sm" variant="secondary" @click="restoreDefaults">
            <template #icon>
              <RotateCcw class="size-3.5" />
            </template>
            恢复默认
          </BaseButton>
          <BaseButton size="sm" variant="primary" :disabled="!dirty" @click="saveTheme">
            <template #icon>
              <Save v-if="dirty" class="size-3.5" />
              <Check v-else class="size-3.5" />
            </template>
            {{ dirty ? '保存并应用' : '已保存' }}
          </BaseButton>
        </div>
      </template>
    </BasePageHeader>

    <section
      class="theme-workbench isolate mt-5 overflow-hidden rounded-cp-card bg-cp-bg-layout shadow-cp-card xl:flex xl:min-h-0 xl:flex-1 xl:flex-col"
      aria-label="主题编辑器"
    >
      <div
        class="theme-workbench-body grid min-h-180 gap-3 xl:min-h-0 xl:flex-1 xl:overflow-hidden xl:grid-cols-[370px_minmax(0,1fr)]"
      >
        <aside
          class="grid h-full min-h-0 grid-rows-[auto_minmax(0,1fr)] gap-3 overflow-hidden rounded-cp-lg bg-cp-bg-container pt-3 pr-0 pb-3 pl-3 shadow-cp-secondary"
          aria-label="主题 Token 编辑面板"
        >
          <BaseInput
            v-model="query"
            class="mr-3"
            aria-label="搜索 Token"
            placeholder="搜索 Seed、Alias 或 Component Token"
          >
            <template #prefix>
              <Search class="size-4" />
            </template>
          </BaseInput>

          <BaseScrollbar class="h-full min-h-0">
            <div class="pr-4">
              <ThemeGlobalTokenPanel
                v-if="scope === 'global'"
                v-model:category="globalCategory"
                :draft="draft"
                :resolved="resolvedPreview"
                :query="query"
                @mode="setMode"
                @preset="selectPreset"
                @primary="setPrimaryColor"
                @seed-color="setSeedColor"
                @seed-number="setSeedNumber"
                @reset-seed="resetSeed"
              />
              <ThemeComponentTokenPanel
                v-else
                v-model="component"
                :draft="draft"
                :resolved="resolvedPreview"
                :query="query"
                @change="setTokenOverride"
                @reset="resetTokenOverride"
                @component-number="setComponentNumber"
                @reset-component-number="resetComponentNumber"
              />
            </div>
          </BaseScrollbar>
        </aside>

        <div class="hidden min-h-0 min-[768px]:block">
          <ThemeEditorPreview v-model="preview" class="h-full" :theme="previewTheme" :style="previewStyle" />
        </div>
      </div>
    </section>
  </div>
</template>
