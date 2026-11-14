<script setup lang="ts">
import { ZConfigProvider } from '@codex-proxy/ui'
import { useDebounceFn, useDevicePixelRatio, useEventListener, useWindowSize } from '@vueuse/core'
import { watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterView } from 'vue-router'
import { applyLocale } from '@/i18n/locale'
import { uiLocale } from '@/i18n/ui'

const { locale } = useI18n({ useScope: 'global' })
const overlay = (navigator as Navigator & { windowControlsOverlay?: { getTitlebarAreaRect: () => DOMRect } }).windowControlsOverlay
if (overlay && matchMedia('(display-mode: standalone), (display-mode: window-controls-overlay)').matches) {
  const root = document.documentElement
  root.classList.add('pwa-window')
  const { height } = useWindowSize({ type: 'outer' })
  const { pixelRatio } = useDevicePixelRatio()
  const rememberHeight = () => {
    const viewport = window.visualViewport?.height ?? window.innerHeight
    const next = viewport - overlay.getTitlebarAreaRect().bottom
    const previous = Number.parseFloat(root.style.getPropertyValue('--app-height'))
    if (!Number.isFinite(previous) || Math.abs(next - previous) >= 1)
      root.style.setProperty('--app-height', `${next}px`)
  }
  // 调整外窗时立即交回 CSS；macOS 还原动画中，外窗和内容视口可能在不同帧才停止变化
  watch([height, pixelRatio], () => root.style.removeProperty('--app-height'))
  const settled = useDebounceFn(rememberHeight, 80)
  useEventListener(window.visualViewport ?? window, 'resize', settled)
  rememberHeight()
}

watch(locale, applyLocale, { immediate: true })
</script>

<template>
  <ZConfigProvider :locale="uiLocale">
    <div class="pwa-titlebar hidden bg-z-bg-container" aria-hidden="true" />
    <RouterView />
  </ZConfigProvider>
</template>

<style scoped>
@media (display-mode: window-controls-overlay) {
  .pwa-titlebar {
    display: block;
    position: fixed;
    z-index: 51;
    top: env(titlebar-area-y);
    left: env(titlebar-area-x);
    width: env(titlebar-area-width);
    height: env(titlebar-area-height);
    -webkit-app-region: drag;
  }
}
</style>
