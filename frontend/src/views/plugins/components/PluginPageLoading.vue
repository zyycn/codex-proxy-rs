<script setup lang="ts">
import { Box } from '@lucide/vue'

import { usePreferredReducedMotion } from '@vueuse/core'
import { gsap } from 'gsap'
import { onMounted, onScopeDispose, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'

const { t } = useI18n({ useScope: 'global' })

const icon = useTemplateRef<HTMLElement>('icon')
const preferredMotion = usePreferredReducedMotion()
let motion: gsap.core.Tween | undefined

function animate() {
  motion?.kill()
  if (!icon.value)
    return
  gsap.set(icon.value, { y: 0 })
  if (preferredMotion.value !== 'reduce') {
    motion = gsap.to(icon.value, {
      y: -5,
      duration: 0.8,
      repeat: -1,
      yoyo: true,
      ease: 'power1.inOut',
    })
  }
}

onMounted(animate)
watch(preferredMotion, animate)
onScopeDispose(() => motion?.kill())
</script>

<template>
  <div class="grid min-h-0 flex-1 place-items-center px-6 py-8" role="status" :aria-label="t('plugins.page.loadingPage')">
    <span ref="icon" class="inline-flex size-16 items-center justify-center text-cp-text-secondary" aria-hidden="true">
      <Box :size="40" :stroke-width="1.5" />
    </span>
  </div>
</template>
