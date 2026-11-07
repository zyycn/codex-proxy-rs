<script setup lang="ts">
import { ZIconButton, ZPopover } from '@codex-proxy/ui'
import { Info, TriangleAlert } from '@lucide/vue'
import { shallowRef, useId } from 'vue'

defineProps<{ label: string, tone?: 'warning' | 'error' }>()

const open = shallowRef(false)
const descriptionId = useId()

function handleKeydown(event: KeyboardEvent) {
  if (event.key !== 'Escape' || !open.value)
    return
  // 先关闭说明，避免一次按键连带关闭正在编辑的配置弹窗。
  event.preventDefault()
  event.stopPropagation()
  open.value = false
}
</script>

<template>
  <ZPopover v-model="open" trigger="hover-click" placement="top-start" :hover-delay="240" class="shrink-0">
    <template #reference>
      <ZIconButton
        variant="link"
        :aria-label="label"
        size="small"
        class="size-5!"
        :aria-expanded="open"
        :aria-describedby="open ? descriptionId : undefined"
        @keydown="handleKeydown"
      >
        <TriangleAlert v-if="tone" class="size-4" :class="tone === 'error' ? 'text-z-error-text' : 'text-z-warning'" />
        <Info v-else class="size-3.5" />
      </ZIconButton>
    </template>
    <div :id="descriptionId" role="tooltip" class="grid max-w-80 gap-2 p-3 text-z-xs leading-relaxed wrap-break-word text-z-text-secondary">
      <slot />
    </div>
  </ZPopover>
</template>
