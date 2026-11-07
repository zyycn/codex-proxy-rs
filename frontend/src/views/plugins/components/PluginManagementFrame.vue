<script setup lang="ts">
import type { PluginManagementPage, PluginManagementView } from '@/api'

import { useEventListener, useResizeObserver } from '@vueuse/core'
import { computed, onMounted, shallowRef, useTemplateRef, watch } from 'vue'
import { useThemeStore } from '@/stores/modules/theme'
import { usePluginFrameSession } from '../composables/usePluginFrameSession'
import PluginExternalLinkDialog from './PluginExternalLinkDialog.vue'
import PluginPageLoading from './PluginPageLoading.vue'

const props = defineProps<{
  view: PluginManagementView
  page: PluginManagementPage
}>()

const emit = defineEmits<{
  stale: []
}>()

const themeStore = useThemeStore()
const containerRef = useTemplateRef<HTMLDivElement>('container')
const iframeRef = useTemplateRef<HTMLIFrameElement>('iframe')
const viewportHeight = shallowRef(0)
const { srcdoc, loading, loadError, contentHeight, externalLink, frameKey, handleFrameLoad, pushLayout, pushTheme } = usePluginFrameSession({
  view: () => props.view,
  page: () => props.page,
  iframe: iframeRef,
  viewportHeight: readViewportHeight,
  onStale: () => emit('stale'),
})
const frameReady = computed(() => contentHeight.value > 0)

function readViewportHeight() {
  const container = containerRef.value
  if (!container)
    return 0

  // 在 iframe 创建前预留可用空间，计算不依赖插件内容高度。
  let top = container.getBoundingClientRect().top
  let bottomPadding = 0
  for (let parent = container.parentElement; parent; parent = parent.parentElement) {
    top += parent.scrollTop
    bottomPadding += Number.parseFloat(getComputedStyle(parent).paddingBottom) || 0
  }
  viewportHeight.value = Math.max(0, Math.floor(document.documentElement.clientHeight - top - bottomPadding))
  return viewportHeight.value
}

useResizeObserver(containerRef, pushLayout)
useEventListener('resize', pushLayout)
watch(
  () => [themeStore.effectiveTheme, themeStore.themeRevision],
  () => void pushTheme(),
)
onMounted(pushLayout)
</script>

<template>
  <div ref="container" class="relative isolate flex min-w-0 w-full" :style="{ minHeight: `${viewportHeight}px` }" :aria-busy="loading || (!frameReady && !loadError)">
    <div v-if="!loadError && (loading || !frameReady)" class="absolute inset-0 z-10 flex bg-z-bg-layout">
      <PluginPageLoading />
    </div>
    <iframe
      v-if="srcdoc && !loadError"
      ref="iframe"
      :key="frameKey"
      :srcdoc="srcdoc"
      :title="`${view.name}：${page.title}`"
      class="block w-full border-0 bg-transparent"
      :style="{ height: `${Math.max(viewportHeight, contentHeight)}px` }"
      :inert="!frameReady"
      :aria-hidden="!frameReady"
      sandbox="allow-scripts"
      referrerpolicy="no-referrer"
      @load="handleFrameLoad"
    />
    <PluginExternalLinkDialog v-if="externalLink" :link="externalLink" @closed="externalLink = undefined" />
  </div>
</template>
