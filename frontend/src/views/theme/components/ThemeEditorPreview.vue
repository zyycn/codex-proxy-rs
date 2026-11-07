<script setup lang="ts">
import type { ThemeScheme } from '@codex-proxy/ui/theme'

import type { CSSProperties } from 'vue'

import type { ThemeEditorPreview } from '../composables/useThemeEditor'
import { ZCard, ZSegmented } from '@codex-proxy/ui'
import { computed, defineAsyncComponent, onBeforeUnmount, onMounted, shallowRef } from 'vue'

import { useI18n } from 'vue-i18n'

import ThemePreviewCanvas from './ThemePreviewCanvas.vue'
import ThemePreviewError from './ThemePreviewError.vue'

defineProps<{
  theme: ThemeScheme
  style: CSSProperties
}>()
const previewReady = shallowRef(false)
const stylesRevision = shallowRef(0)
let paintFrame = 0

// 先绘制画布和控件，再挂载预览；缓存命中时也不阻塞外框首帧
onMounted(() => {
  paintFrame = requestAnimationFrame(() => {
    paintFrame = requestAnimationFrame(() => {
      previewReady.value = true
    })
  })
})
onBeforeUnmount(() => cancelAnimationFrame(paintFrame))

const ThemeDashboardPreview = defineAsyncComponent({
  loader: async () => {
    const component = await import('./ThemeDashboardPreview.vue')
    stylesRevision.value++
    return component
  },
  errorComponent: ThemePreviewError,
  timeout: 30000,
})
const ThemeComponentPreview = defineAsyncComponent({
  loader: async () => {
    const component = await import('./ThemeComponentPreview.vue')
    stylesRevision.value++
    return component
  },
  errorComponent: ThemePreviewError,
  timeout: 30000,
})

const { t } = useI18n({ useScope: 'global' })

const preview = defineModel<ThemeEditorPreview>({ required: true })

const previewOptions = computed(() => ([
  {
    label: t('appearance.preview.homeArtboard'),
    value: 'page',
  },
  {
    label: t('appearance.preview.components.componentOverview'),
    value: 'components',
  },
]))
</script>

<template>
  <section class="grid h-full min-h-0 grid-rows-[auto_minmax(0,1fr)] gap-3 overflow-hidden" :aria-label="t('appearance.preview.themeRealTimePreview')">
    <ZCard as="header" padding="none" class="shrink-0 flex flex-wrap items-center justify-between gap-3 px-3 py-2.5">
      <div>
        <strong class="block text-z-sm font-heavy text-z-text">{{ t('appearance.preview.livePreview') }}</strong>
        <span class="mt-0.5 block text-[9px] font-emphasis text-z-text-quaternary">{{ t('appearance.preview.description') }}</span>
      </div>
      <ZSegmented v-model="preview" :aria-label="t('appearance.preview.previewType')" size="small" :options="previewOptions" />
    </ZCard>

    <ThemePreviewCanvas
      class="h-full min-h-0"
      :theme="theme"
      :style="style"
      :styles-revision="stylesRevision"
    >
      <template v-if="previewReady">
        <ThemeDashboardPreview v-if="preview === 'page'" />
        <ThemeComponentPreview v-else />
      </template>
    </ThemePreviewCanvas>
  </section>
</template>
