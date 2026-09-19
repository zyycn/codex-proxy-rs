<script setup lang="ts">
import type { TableColumnOption } from './useTableColumns'
import { Columns3, GripVertical } from '@lucide/vue'
import { usePreferredReducedMotion } from '@vueuse/core'
import { moveArrayElement, useSortable } from '@vueuse/integrations/useSortable'
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
  reorder: [keys: string[]]
  reset: []
}>()

const open = shallowRef(false)
const panelId = useId()
const triggerRef = useTemplateRef<InstanceType<typeof BaseIconButton>>('trigger')
const panelRef = useTemplateRef<HTMLDivElement>('panel')
const listRef = useTemplateRef<HTMLDivElement>('list')
const visibleCount = computed(() => props.options.filter(option => option.visible).length)
const dragging = shallowRef(false)
const hoveredColumnKey = shallowRef<string>()
const announcement = shallowRef('')
const preferredMotion = usePreferredReducedMotion()
const reorderAnimation = computed(() => preferredMotion.value === 'reduce' ? 0 : 200)
const sortableOptions = computed({
  get: () => props.options,
  set: options => emit('reorder', options.map(option => option.key)),
})

const { option: setSortableOption } = useSortable(listRef, sortableOptions, {
  watchElement: true,
  animation: reorderAnimation.value,
  easing: 'cubic-bezier(0.2, 0, 0, 1)',
  handle: '[data-column-handle]',
  draggable: '[data-reorderable="true"]',
  ghostClass: 'opacity-40',
  onMove: event => event.related.dataset.reorderable !== 'false',
  onChoose: (event) => {
    dragging.value = true
    hoveredColumnKey.value = event.item.dataset.columnKey
  },
  onUnchoose: () => { dragging.value = false },
  onEnd: (event) => {
    const key = event.item.dataset.columnKey
    hoveredColumnKey.value = key
    if (key && event.oldIndex !== event.newIndex) {
      // useSortable 在下一次更新中同步数组，再等待父组件应用列顺序后恢复焦点。
      void nextTick(() => focusMovedColumn(key))
    }
  },
})

function updateHoveredColumn(event: PointerEvent) {
  if (dragging.value || !(event.target instanceof Element))
    return

  const item = event.target.closest<HTMLElement>('[role="listitem"][data-column-key]')
  const option = props.options.find(option => option.key === item?.dataset.columnKey)
  // 原生拖拽结束后 :hover 可能仍停在旧位置，只在真实指针移动时更新高亮。
  hoveredColumnKey.value = option && (!option.disabled || option.reorderable) ? option.key : undefined
}

function clearHoveredColumn() {
  if (!dragging.value)
    hoveredColumnKey.value = undefined
}

watch([listRef, reorderAnimation], () => {
  setSortableOption('animation', reorderAnimation.value)
}, { flush: 'post' })

async function focusMovedColumn(key: string) {
  await nextTick()
  if (!open.value)
    return

  const handles = panelRef.value?.querySelectorAll<HTMLButtonElement>('[data-column-handle]')
  const handle = Array.from(handles ?? []).find(handle => handle.dataset.columnKey === key)
  handle?.focus({ preventScroll: true })
  handle?.scrollIntoView({ block: 'nearest' })
  const index = props.options.findIndex(option => option.key === key)
  const option = props.options[index]
  if (option)
    announcement.value = `${option.label}已移到第 ${index + 1} 项`
}

function moveWithKeyboard(key: string, direction: -1 | 1) {
  const options = props.options.filter(option => option.reorderable)
  const index = options.findIndex(option => option.key === key)
  const target = options[index + direction]
  if (index < 0 || !target)
    return

  moveArrayElement(sortableOptions, props.options.indexOf(options[index]!), props.options.indexOf(target))
  void nextTick(() => focusMovedColumn(key))
}

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
  if (!dragging.value && event.relatedTarget instanceof Node
    && !panelRef.value?.contains(event.relatedTarget)
    && !triggerRef.value?.$el.contains(event.relatedTarget)) {
    open.value = false
  }
}

watch(open, async (value) => {
  if (!value) {
    dragging.value = false
    hoveredColumnKey.value = undefined
    announcement.value = ''
    return
  }
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
        <p :id="`${panelId}-reorder-help`" class="sr-only">
          拖动手柄调整列顺序，也可聚焦手柄后使用上下方向键移动。
        </p>
        <p class="sr-only" role="status" aria-live="polite">
          {{ announcement }}
        </p>
        <BaseScrollbar max-height="min(20rem, calc(100dvh - 10rem))">
          <div
            ref="list"
            class="grid gap-0.5 p-2 [&_[data-column-handle]:is(:hover,:active)]:bg-transparent"
            role="list"
            aria-label="表格列"
            @pointermove="updateHoveredColumn"
            @pointerleave="clearHoveredColumn"
          >
            <div
              v-for="option in options"
              :key="option.key"
              role="listitem"
              :data-column-key="option.key"
              :data-reorderable="option.reorderable"
              :data-highlighted="hoveredColumnKey === option.key"
              class="flex min-h-9 items-center rounded-cp pr-1 data-[highlighted=true]:bg-cp-bg-text-hover"
            >
              <BaseCheckbox
                :model-value="option.visible"
                :label="option.label"
                :disabled="option.disabled"
                show-label
                class="min-h-9 min-w-0 flex-1 px-2 py-2"
                @update:model-value="emit('change', option.key, $event)"
              />
              <BaseIconButton
                v-if="option.reorderable"
                :label="`拖动调整${option.label}顺序`"
                :aria-describedby="`${panelId}-reorder-help`"
                :data-column-key="option.key"
                data-column-handle
                variant="ghost"
                size="sm"
                class="touch-none cursor-grab active:cursor-grabbing"
                @keydown.up.stop.prevent="moveWithKeyboard(option.key, -1)"
                @keydown.down.stop.prevent="moveWithKeyboard(option.key, 1)"
              >
                <GripVertical class="size-3.5" aria-hidden="true" />
              </BaseIconButton>
            </div>
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
