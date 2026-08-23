<script setup lang="ts">
import type { CSSProperties } from 'vue'
import type { ThemeColorTokens } from '@/composables/useThemeColor'
import type { ThemeName } from '@/theme'

import { computed, onMounted, shallowRef, useTemplateRef } from 'vue'

import { provideThemeColorTokens } from '@/composables/useThemeColor'

const props = defineProps<{
  theme: ThemeName
  style: CSSProperties
}>()

const host = useTemplateRef<HTMLElement>('host')
const contentRoot = shallowRef<HTMLElement | null>(null)
const themeTokens = computed<ThemeColorTokens>(() => props.style as ThemeColorTokens)

provideThemeColorTokens(themeTokens)

function serializeDocumentStyles() {
  return Array.from(document.styleSheets, (styleSheet) => {
    try {
      return Array.from(styleSheet.cssRules, rule => rule.cssText).join('\n')
    }
    catch {
      return ''
    }
  }).filter(Boolean).join('\n')
}

onMounted(() => {
  if (!host.value)
    return

  const shadowRoot = host.value.attachShadow({ mode: 'open' })
  const applicationStyles = document.createElement('style')
  const previewRoot = document.createElement('div')

  applicationStyles.dataset.themePreviewStyles = ''
  applicationStyles.textContent = serializeDocumentStyles()
  previewRoot.dataset.themePreviewRoot = ''

  shadowRoot.append(applicationStyles, previewRoot)
  contentRoot.value = previewRoot
})
</script>

<template>
  <div
    ref="host"
    class="block h-full min-h-0 w-full min-w-0"
    data-theme-preview-shadow-host
  >
    <Teleport v-if="contentRoot" :to="contentRoot">
      <div
        class="h-full min-h-0 w-full min-w-0 bg-cp-bg-layout font-sans text-cp-text"
        :data-theme="theme"
        :style="style"
      >
        <slot />
      </div>
    </Teleport>
  </div>
</template>
