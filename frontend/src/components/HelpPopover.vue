<script setup lang="ts">
import { ZIconButton, ZPopover } from '@codex-proxy/ui'
import { CircleAlert } from '@lucide/vue'

defineProps<{ label: string, content: string }>()
</script>

<template>
  <ZPopover class="h-4 items-center" trigger="hover-click" placement="top-start" :hover-delay="240">
    <template #reference="{ open }">
      <ZIconButton variant="link" size="small" :aria-label="label" :aria-expanded="open" aria-haspopup="dialog">
        <CircleAlert class="size-3.5" aria-hidden="true" />
      </ZIconButton>
    </template>
    <section role="dialog" :aria-label="label" class="max-w-72 p-3 text-z-xs leading-relaxed text-z-text-secondary">
      {{ content }}
    </section>
  </ZPopover>
</template>
