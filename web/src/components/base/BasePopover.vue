<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, useAttrs, watch } from 'vue'
import type { CSSProperties } from 'vue'

type PopoverPlacement = 'bottom-start' | 'bottom-end' | 'top-start' | 'top-end'

defineOptions({
  inheritAttrs: false,
})

const props = withDefaults(
  defineProps<{
    placement?: PopoverPlacement
    offset?: number
    width?: number | string
    disabled?: boolean
    panelClass?: string
  }>(),
  {
    placement: 'bottom-end',
    offset: 6,
    disabled: false,
    panelClass: '',
  },
)

const open = defineModel<boolean>({ default: false })
const attrs = useAttrs()

const rootRef = ref<HTMLElement | null>(null)
const triggerRef = ref<HTMLElement | null>(null)
const popoverRef = ref<HTMLElement | null>(null)
const popoverStyle = ref<CSSProperties>({})

const popoverClasses = computed(() => [
  'fixed z-50 overflow-visible rounded-(--cp-popover-radius) border-0 bg-(--cp-bg-surface) p-1.5 text-left shadow-(--cp-shadow-popover)',
  props.panelClass,
])

const sizeStyle = computed<CSSProperties>(() => {
  const width = toCssLength(props.width)
  return width ? { width } : {}
})

function toCssLength(value?: number | string) {
  if (typeof value === 'number') {
    return `${value}px`
  }

  const trimmed = value?.trim()
  return trimmed || undefined
}

function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(value, max))
}

function updatePopoverPosition() {
  if (!open.value || !triggerRef.value) return

  const viewportPadding = 8
  const triggerRect = triggerRef.value.getBoundingClientRect()
  const panelRect = popoverRef.value?.getBoundingClientRect()
  const panelWidth = Math.max(
    panelRect?.width ?? 0,
    toCssLength(props.width) ? 0 : triggerRect.width,
  )
  const panelHeight = panelRect?.height ?? 0
  const belowSpace = window.innerHeight - triggerRect.bottom - props.offset
  const aboveSpace = triggerRect.top - props.offset
  const prefersTop = props.placement.startsWith('top')
  const placeAbove = prefersTop
    ? !(aboveSpace < panelHeight && belowSpace > aboveSpace)
    : belowSpace < panelHeight && aboveSpace > belowSpace
  const rawTop = placeAbove
    ? triggerRect.top - panelHeight - props.offset
    : triggerRect.bottom + props.offset
  const maxTop = Math.max(viewportPadding, window.innerHeight - panelHeight - viewportPadding)
  const top = panelHeight > 0 ? clamp(rawTop, viewportPadding, maxTop) : rawTop
  const rawLeft = props.placement.endsWith('end')
    ? triggerRect.right - panelWidth
    : triggerRect.left
  const maxLeft = Math.max(viewportPadding, window.innerWidth - panelWidth - viewportPadding)
  const left = clamp(rawLeft, viewportPadding, maxLeft)

  popoverStyle.value = {
    left: `${left}px`,
    top: `${top}px`,
    maxWidth: `calc(100vw - ${viewportPadding * 2}px)`,
    ...(toCssLength(props.width) ? {} : { minWidth: `${triggerRect.width}px` }),
  }
}

async function openPopover() {
  if (props.disabled || open.value) return

  open.value = true
  await nextTick()
  updatePopoverPosition()
}

function closePopover() {
  open.value = false
}

function togglePopover() {
  if (open.value) {
    closePopover()
    return
  }

  void openPopover()
}

function handleTriggerKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    closePopover()
  }
}

function handleDocumentKeydown(event: KeyboardEvent) {
  if (!open.value || event.key !== 'Escape') return
  closePopover()
}

function handlePointerDown(event: PointerEvent) {
  const target = event.target as Node | null
  if (!target) return

  if (rootRef.value?.contains(target) || popoverRef.value?.contains(target)) return
  closePopover()
}

watch(open, async (isOpen) => {
  if (!isOpen) return

  await nextTick()
  updatePopoverPosition()
})

onMounted(() => {
  document.addEventListener('pointerdown', handlePointerDown)
  document.addEventListener('keydown', handleDocumentKeydown)
  window.addEventListener('resize', updatePopoverPosition)
  window.addEventListener('scroll', updatePopoverPosition, true)
})

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', handlePointerDown)
  document.removeEventListener('keydown', handleDocumentKeydown)
  window.removeEventListener('resize', updatePopoverPosition)
  window.removeEventListener('scroll', updatePopoverPosition, true)
})
</script>

<template>
  <div ref="rootRef" class="relative inline-block overflow-visible" v-bind="attrs">
    <div
      ref="triggerRef"
      class="inline-flex"
      @click.stop="togglePopover"
      @keydown="handleTriggerKeydown"
    >
      <slot name="trigger" :open="open" :close="closePopover" :toggle="togglePopover" />
    </div>

    <Teleport to="body">
      <Transition
        enter-active-class="transition-[opacity,transform] duration-150 ease-out"
        enter-from-class="-translate-y-1 opacity-0"
        enter-to-class="translate-y-0 opacity-100"
        leave-active-class="transition-opacity duration-150 ease-in"
        leave-from-class="opacity-100"
        leave-to-class="opacity-0"
      >
        <div
          v-if="open"
          ref="popoverRef"
          :class="popoverClasses"
          :style="[sizeStyle, popoverStyle]"
        >
          <slot :open="open" :close="closePopover" :toggle="togglePopover" />
        </div>
      </Transition>
    </Teleport>
  </div>
</template>
