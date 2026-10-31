<script setup lang="ts">
import { ZPopover, ZScrollbar } from '@codex-proxy/ui'

import { useEventListener } from '@vueuse/core'

import { computed, shallowRef, useId, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  notes: string
}>()

const { t } = useI18n({ useScope: 'global' })

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
  <ZPopover
    :model-value="open"
    class="min-w-0 max-w-full"
    trigger="hover-click"
    placement="right"
    :offset="12"
    :hover-delay="240"
    @update:model-value="updateOpen"
  >
    <template #reference>
      <button
        type="button"
        class="inline-flex min-w-0 max-w-full cursor-default touch-manipulation items-center rounded-cp-sm border-0 bg-transparent p-0 text-left font-mono text-cp-xs leading-4 font-emphasis text-cp-text-quaternary outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
        :aria-label="t('accounts.notes.viewAccountNotes')"
        aria-haspopup="dialog"
        :aria-expanded="open"
        :aria-controls="open ? detailId : undefined"
      >
        <span ref="notesText" class="min-w-0 truncate">{{ notes }}</span>
      </button>
    </template>

    <section :id="detailId" class="w-max max-w-[min(20rem,calc(100vw-1rem))] overflow-hidden rounded-cp-lg" role="dialog" :aria-label="t('accounts.notes.title')">
      <ZScrollbar max-height="min(240px, calc(100dvh - 2rem))">
        <div class="px-3 py-2.5 text-cp-sm leading-5 whitespace-pre-wrap text-cp-text select-text wrap-anywhere">
          {{ notes }}
        </div>
      </ZScrollbar>
    </section>
  </ZPopover>
</template>
