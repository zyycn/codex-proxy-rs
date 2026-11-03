<script setup lang="ts">
import type { ThemeScheme } from '@codex-proxy/ui/theme'
import type { CSSProperties } from 'vue'
import type { ThemeColorTokens } from '@/composables/useThemeColor'

import { computed, onMounted, shallowRef, useTemplateRef, watch } from 'vue'

import { provideThemeColorTokens } from '@/composables/useThemeColor'

const props = defineProps<{
  theme: ThemeScheme
  style: CSSProperties
  stylesRevision: number
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

let applicationStyles: HTMLStyleElement | undefined

function refreshStyles() {
  if (applicationStyles)
    applicationStyles.textContent = serializeDocumentStyles()
}

// 异步预览的 CSS 到齐后同步影子根，避免只复制首帧样式而丢失后加载的规则
watch(() => props.stylesRevision, refreshStyles, { flush: 'post' })

onMounted(() => {
  if (!host.value)
    return

  const shadowRoot = host.value.attachShadow({ mode: 'open' })
  applicationStyles = document.createElement('style')
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
        class="h-full min-h-0 w-full min-w-0 font-sans text-z-text"
        :data-theme="theme"
        :style="style"
      >
        <slot />
      </div>
    </Teleport>
  </div>
</template>
