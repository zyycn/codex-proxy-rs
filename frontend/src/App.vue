<script setup lang="ts">
import { ZConfigProvider } from '@codex-proxy/ui'
import { useDebounceFn, useDevicePixelRatio, useEventListener, useWindowSize } from '@vueuse/core'
import { watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterView } from 'vue-router'
import { applyLocale } from '@/i18n/locale'
import { uiLocale } from '@/i18n/ui'

const { locale } = useI18n({ useScope: 'global' })
const overlay = (navigator as Navigator & { windowControlsOverlay?: EventTarget & { getTitlebarAreaRect: () => DOMRect } }).windowControlsOverlay
if (overlay) {
  const root = document.documentElement
  root.classList.add('supports-titlebar-overlay')
  const inAppWindow = matchMedia('(display-mode: standalone), (display-mode: window-controls-overlay)')
  const { height } = useWindowSize({ type: 'outer' })
  const { pixelRatio } = useDevicePixelRatio()
  const rememberHeight = useDebounceFn(() => {
    if (!inAppWindow.matches) {
      root.style.removeProperty('--settled-app-height')
      return
    }
    const viewport = window.visualViewport?.height ?? window.innerHeight
    const next = viewport - overlay.getTitlebarAreaRect().bottom
    const previous = Number.parseFloat(root.style.getPropertyValue('--settled-app-height'))
    if (!Number.isFinite(previous) || Math.abs(next - previous) >= 1)
      root.style.setProperty('--settled-app-height', `${next}px`)
  }, 80)
  // 调整外窗时先恢复 CSS 自动布局，待视口和标题栏都稳定后再记录正文高度
  const resetHeight = () => {
    root.style.removeProperty('--settled-app-height')
    rememberHeight()
  }
  watch([height, pixelRatio], resetHeight)
  useEventListener(inAppWindow, 'change', resetHeight)
  useEventListener(overlay, 'geometrychange', rememberHeight)
  useEventListener(window.visualViewport ?? window, 'resize', rememberHeight)
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
