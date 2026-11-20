<script setup lang="ts">
import { ZIconButton, ZPopover } from '@codex-proxy/ui'
import { CircleAlert } from '@lucide/vue'
import { useMediaQuery } from '@vueuse/core'

defineProps<{ label: string, content?: string }>()

const canHover = useMediaQuery('(hover: hover)')
</script>

<template>
  <ZPopover class="h-4 items-center" :trigger="canHover ? 'hover-click' : 'click'" placement="top-start" :hover-delay="240">
    <template #reference="{ open }">
      <ZIconButton variant="link" size="small" :aria-label="label" :aria-expanded="open" aria-haspopup="dialog">
        <CircleAlert class="size-3.5" aria-hidden="true" />
      </ZIconButton>
    </template>
    <section role="dialog" :aria-label="label" class="max-w-72 p-3 text-z-xs leading-relaxed text-z-text-secondary">
      <slot>{{ content }}</slot>
    </section>
  </ZPopover>
</template>
