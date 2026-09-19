<script setup lang="ts">
import { useEventListener } from '@vueuse/core'
import { computed, shallowRef, useId, useTemplateRef, watch } from 'vue'

import BasePopover from '@/components/base/BasePopover.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'

const props = defineProps<{
  notes: string
}>()

const detailId = `account-notes-${useId()}`
const notesTextRef = useTemplateRef<HTMLSpanElement>('notesText')
const open = shallowRef(false)
const viewportTarget = computed(() => open.value ? window : null)

function updateOpen(nextOpen: boolean) {
  const text = notesTextRef.value
  // 在实际打开前按需测量，悬停、点击和键盘入口共用同一道溢出检查。
  open.value = nextOpen && Boolean(text && text.scrollWidth > text.clientWidth)
}

function recheckOpen() {
  if (open.value)
    updateOpen(true)
}

useEventListener(viewportTarget, 'resize', recheckOpen)
watch(() => props.notes, recheckOpen, { flush: 'post' })
</script>

<template>
  <BasePopover
    :model-value="open"
    class="min-w-0 max-w-full"
    trigger="hover-click"
    placement="right"
    :offset="12"
    :hover-delay="240"
    @update:model-value="updateOpen"
  >
    <template #trigger>
      <button
        type="button"
        class="inline-flex min-w-0 max-w-full touch-manipulation items-center rounded-cp-sm border-0 bg-transparent p-0 text-left text-cp-xs leading-4 text-cp-text-tertiary outline-none transition-colors hover:text-cp-primary-text focus-visible:ring-2 focus-visible:ring-cp-control-outline motion-reduce:transition-none"
        :class="open ? 'text-cp-primary-text' : ''"
        aria-label="查看账号备注"
        aria-haspopup="dialog"
        :aria-expanded="open"
        :aria-controls="open ? detailId : undefined"
      >
        <span ref="notesText" class="min-w-0 truncate">{{ notes }}</span>
      </button>
    </template>

    <section :id="detailId" class="w-max max-w-[min(20rem,calc(100vw-1rem))] overflow-hidden rounded-cp-lg" role="dialog" aria-label="账号备注">
      <BaseScrollbar max-height="min(240px, calc(100dvh - 2rem))">
        <div class="px-3 py-2.5 text-cp-sm leading-5 whitespace-pre-wrap text-cp-text select-text wrap-anywhere">
          {{ notes }}
        </div>
      </BaseScrollbar>
    </section>
  </BasePopover>
</template>
