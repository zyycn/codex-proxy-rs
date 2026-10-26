<script setup lang="ts">
import { ZButton, ZInput, ZPageHeader, ZScrollbar, ZSegmented, ZSkeleton } from '@codex-proxy/ui'
import { Check, Eye, Pencil, RotateCcw, Save, Search, Undo2 } from '@lucide/vue'

import { computed, defineAsyncComponent, shallowRef } from 'vue'

import { useI18n } from 'vue-i18n'

import { notification as ZNotification } from '@/utils/notification'

import { useThemeEditor } from '../composables/useThemeEditor'
import ThemeComponentTokenPanel from './ThemeComponentTokenPanel.vue'
import ThemeGlobalTokenPanel from './ThemeGlobalTokenPanel.vue'

const ThemeEditorPreview = defineAsyncComponent({
  loader: () => import('./ThemeEditorPreview.vue'),
  loadingComponent: ZSkeleton,
  delay: 200,
  timeout: 30000,
})

const { t } = useI18n({ useScope: 'global' })
const mobilePreviewOpen = shallowRef(false)

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

const scopeOptions = computed(() => ([
  {
    label: t('appearance.settings.global'),
    value: 'global',
  },
  {
    label: t('appearance.settings.components'),
    value: 'component',
  },
]))

function switchScope(value: string) {
  scope.value = value === 'component'
    ? 'component'
    : 'global'
  mobilePreviewOpen.value = false
}

function saveTheme(event: MouseEvent) {
  if (!save(event)) {
    ZNotification.error({
      message: t('appearance.settings.invalidTheme'),
    })
    return
  }
  ZNotification.success({
    message: t('appearance.settings.themeSavedAndApplied'),
  })
}
</script>

<template>
  <div class="@container flex min-h-0 min-w-0 w-full flex-col">
    <ZPageHeader :title="t('navigation.appearance')">
      <template #description>
        <span class="flex flex-wrap items-center gap-x-2 gap-y-1">
          <span>{{ t('appearance.settings.description') }}</span>
          <span
            class="inline-flex items-center gap-1.5 text-cp-sm font-heavy"
            :class="dirty ? 'text-cp-primary-text' : 'text-cp-text-tertiary'"
          >
            <span class="size-1.5 rounded-full bg-current" />
            {{ dirty ? t('appearance.settings.pendingChanges', { count: modificationCount }) : t('appearance.settings.synced') }}
          </span>
        </span>
      </template>

      <template #actions>
        <div class="flex max-w-[100cqw] flex-wrap items-center justify-end gap-2">
          <ZSegmented
            :model-value="scope"
            :aria-label="t('appearance.settings.themeEditingLevel')"
            :options="scopeOptions"
            size="small"
            @update:model-value="switchScope"
          />
          <ZButton size="small" class="min-[768px]:hidden" @click="mobilePreviewOpen = !mobilePreviewOpen">
            <template #icon>
              <Pencil v-if="mobilePreviewOpen" class="size-3.5" />
              <Eye v-else class="size-3.5" />
            </template>
            {{ mobilePreviewOpen ? t('appearance.preview.edit') : t('appearance.preview.open') }}
          </ZButton>
          <ZButton size="small" variant="text" :disabled="!dirty" @click="resetDraft">
            <template #icon>
              <Undo2 class="size-3.5" />
            </template>
            {{ t('appearance.settings.undoDraft') }}
          </ZButton>
          <ZButton size="small" @click="restoreDefaults">
            <template #icon>
              <RotateCcw class="size-3.5" />
            </template>
            {{ t('appearance.settings.restoreDefault') }}
          </ZButton>
          <ZButton size="small" type="primary" :disabled="!dirty" @click="saveTheme">
            <template #icon>
              <Save v-if="dirty" class="size-3.5" />
              <Check v-else class="size-3.5" />
            </template>
            {{ dirty ? t('appearance.settings.saveAndApply') : t('appearance.settings.saved') }}
          </ZButton>
        </div>
      </template>
    </ZPageHeader>

    <section
      class="theme-workbench isolate mt-5 overflow-hidden rounded-cp-card bg-cp-bg-layout shadow-cp-card xl:flex xl:min-h-0 xl:flex-1 xl:flex-col"
      :aria-label="t('appearance.settings.themeEditor')"
    >
      <div
        class="theme-workbench-body grid min-h-180 gap-3 xl:min-h-0 xl:flex-1 xl:overflow-hidden xl:grid-cols-[370px_minmax(0,1fr)]"
      >
        <aside
          class="h-full min-h-0 grid-rows-[auto_minmax(0,1fr)] gap-3 overflow-hidden rounded-cp-lg bg-cp-bg-container pt-3 pr-0 pb-3 pl-3 shadow-cp-secondary min-[768px]:grid"
          :class="mobilePreviewOpen ? 'hidden' : 'grid'"
          :aria-label="t('appearance.settings.themeTokenEditingPanel')"
        >
          <ZInput
            v-model="query"
            class="pr-3"
            :aria-label="t('appearance.settings.searchToken')"
            :placeholder="t('appearance.settings.searchPlaceholder')"
          >
            <template #prefix>
              <Search class="size-4" />
            </template>
          </ZInput>

          <ZScrollbar class="h-full min-h-0">
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
          </ZScrollbar>
        </aside>

        <div class="min-h-0 min-[768px]:block" :class="mobilePreviewOpen ? 'block' : 'hidden'">
          <ThemeEditorPreview v-model="preview" class="h-full" :theme="previewTheme" :style="previewStyle" />
        </div>
      </div>
    </section>
  </div>
</template>
