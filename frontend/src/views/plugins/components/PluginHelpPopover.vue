<script setup lang="ts">
import { BaseIconButton, BasePopover } from '@codex-proxy/ui'
import { CircleAlert, Info, TriangleAlert } from '@lucide/vue'
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
  <BasePopover v-model="open" trigger="hover-click" placement="top-start" :hover-delay="240" class="shrink-0">
    <template #trigger>
      <BaseIconButton
        :label="label"
        size="sm"
        class="size-5!"
        :title="undefined"
        :aria-expanded="open"
        :aria-describedby="open ? descriptionId : undefined"
        @keydown="handleKeydown"
      >
        <CircleAlert v-if="tone === 'error'" class="size-4 text-cp-error-text" />
        <TriangleAlert v-else-if="tone === 'warning'" class="size-4 text-cp-warning" />
        <Info v-else class="size-3.5" />
      </BaseIconButton>
    </template>
    <div :id="descriptionId" role="tooltip" class="grid max-w-80 gap-2 p-3 text-cp-xs leading-relaxed break-words text-cp-text-secondary">
      <slot />
    </div>
  </BasePopover>
</template>
