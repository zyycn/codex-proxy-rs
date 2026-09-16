<script setup lang="ts">
import type { TableColumnOption } from './useTableColumns'
import { Columns3 } from '@lucide/vue'
import { computed, nextTick, shallowRef, useId, useTemplateRef, watch } from 'vue'
import BaseButton from '../BaseButton.vue'
import BaseCheckbox from '../BaseCheckbox.vue'
import BaseIconButton from '../BaseIconButton.vue'
import BasePopover from '../BasePopover.vue'
import BaseScrollbar from '../BaseScrollbar.vue'

const props = defineProps<{
  options: TableColumnOption[]
}>()

const emit = defineEmits<{
  change: [key: string, visible: boolean]
  reset: []
}>()

const open = shallowRef(false)
const panelId = useId()
const triggerRef = useTemplateRef<InstanceType<typeof BaseIconButton>>('trigger')
const panelRef = useTemplateRef<HTMLDivElement>('panel')
const visibleCount = computed(() => props.options.filter(option => option.visible).length)

function closeAndFocus() {
  open.value = false
  triggerRef.value?.$el.focus()
}

function handleTab(event: KeyboardEvent) {
  const controls = panelRef.value?.querySelectorAll<HTMLElement>('input:not(:disabled), button:not(:disabled)')
  if (!controls?.length)
    return

  const edge = event.shiftKey ? controls[0] : controls[controls.length - 1]
  if (document.activeElement !== edge)
    return

  // 浮层挂在 body 下；边界处回到触发按钮，继续工具栏原有的 Tab 顺序。
  if (event.shiftKey)
    event.preventDefault()
  closeAndFocus()
}

function handleFocusOut(event: FocusEvent) {
  if (event.relatedTarget instanceof Node
    && !panelRef.value?.contains(event.relatedTarget)
    && !triggerRef.value?.$el.contains(event.relatedTarget)) {
    open.value = false
  }
}

watch(open, async (value) => {
  if (!value)
    return
  await nextTick()
  panelRef.value?.querySelector<HTMLElement>('input:not(:disabled), button:not(:disabled)')?.focus({ preventScroll: true })
})
</script>

<template>
  <BasePopover
    v-model="open"
    placement="bottom-end"
    :arrow-surface-class="{ top: 'bg-(--cp-popover-header-bg)' }"
  >
    <template #trigger>
      <BaseIconButton
        ref="trigger"
        label="显示列"
        variant="filled"
        :pressed="open"
        aria-haspopup="dialog"
        :aria-expanded="open"
        :aria-controls="open ? panelId : undefined"
      >
        <Columns3 class="size-4.5" aria-hidden="true" />
      </BaseIconButton>
    </template>

    <div
      role="presentation"
      @keydown.esc.stop.prevent="closeAndFocus"
      @keydown.tab="handleTab"
      @focusout="handleFocusOut"
    >
      <div
        :id="panelId"
        ref="panel"
        role="dialog"
        aria-label="显示列"
        class="w-60 max-w-full overflow-hidden rounded-cp-lg"
      >
        <div class="flex items-center justify-between gap-3 bg-cp-popover-header-bg px-3 py-2.5">
          <span class="text-cp-sm font-bold text-cp-text">显示列</span>
          <span class="text-cp-xs text-cp-text-secondary">{{ visibleCount }} / {{ options.length }}</span>
        </div>
        <BaseScrollbar max-height="min(20rem, calc(100dvh - 10rem))">
          <div class="grid gap-0.5 p-2">
            <BaseCheckbox
              v-for="option in options"
              :key="option.key"
              :model-value="option.visible"
              :label="option.label"
              :disabled="option.disabled"
              show-label
              class="min-h-9 w-full rounded-cp px-2 py-2"
              :class="option.disabled ? undefined : 'hover:bg-cp-bg-text-hover'"
              @update:model-value="emit('change', option.key, $event)"
            />
          </div>
        </BaseScrollbar>
        <div class="flex justify-end px-2 pb-2">
          <BaseButton variant="ghost" size="sm" @click="emit('reset')">
            恢复默认
          </BaseButton>
        </div>
      </div>
    </div>
  </BasePopover>
</template>
