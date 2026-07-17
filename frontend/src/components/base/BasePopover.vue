<script setup lang="ts">
import type { CSSProperties } from 'vue'
import { onClickOutside, useEventListener, useThrottleFn, whenever } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { computed, nextTick, onBeforeUnmount, ref, shallowRef, useAttrs, watch } from 'vue'

type PopoverPlacement
  = 'top' | 'top-start' | 'top-end' | 'right' | 'bottom' | 'bottom-start' | 'bottom-end' | 'left'
type PopoverTrigger = 'click' | 'hover'

interface PopoverPoint {
  left: number
  top: number
}

interface PopoverPosition {
  placement: PopoverPlacement
  point: PopoverPoint
}

defineOptions({
  inheritAttrs: false,
})

const props = withDefaults(
  defineProps<{
    placement?: PopoverPlacement
    trigger?: PopoverTrigger
    offset?: number
    width?: number | string
    disabled?: boolean
    panelClass?: string
    triggerClass?: string
    anchorElement?: HTMLElement | null
    animatePosition?: boolean
  }>(),
  {
    placement: 'bottom-end',
    trigger: 'click',
    offset: 6,
    disabled: false,
    panelClass: '',
    triggerClass: '',
    anchorElement: null,
    animatePosition: false,
  },
)

const open = defineModel<boolean>({ default: false })
const attrs = useAttrs()

const rootRef = ref<HTMLElement | null>(null)
const triggerRef = ref<HTMLElement | null>(null)
const popoverRef = ref<HTMLElement | null>(null)
const popoverStyle = ref<CSSProperties>({})
const popoverArrowStyle = ref<CSSProperties>({})
const hoverCloseTimer = shallowRef<number>()
const viewportTarget = computed(() => (open.value && typeof window !== 'undefined' ? window : null))

const popoverClasses = computed(() => [
  'fixed z-50 overflow-visible rounded-(--cp-popover-radius) border-0 bg-(--cp-bg-surface) p-1.5 text-left shadow-(--cp-shadow-popover)',
  props.animatePosition
    ? 'transition-[left,top] duration-150 ease-out motion-reduce:transition-none'
    : undefined,
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

function updatePopoverPosition() {
  const anchorElement = props.anchorElement ?? triggerRef.value
  if (!open.value || !anchorElement)
    return

  const viewportPadding = 8
  const triggerRect = anchorElement.getBoundingClientRect()
  const panelRect = popoverRef.value?.getBoundingClientRect()
  const panelWidth = Math.max(
    panelRect?.width ?? 0,
    toCssLength(props.width) ? 0 : triggerRect.width,
  )
  const panelHeight = panelRect?.height ?? 0
  const maxLeft = Math.max(viewportPadding, window.innerWidth - panelWidth - viewportPadding)
  const maxTop = Math.max(viewportPadding, window.innerHeight - panelHeight - viewportPadding)
  const position = choosePopoverPosition({
    placement: props.placement,
    triggerRect,
    panelWidth,
    panelHeight,
    offset: props.offset,
    viewportPadding,
  })
  const left = clamp(position.point.left, viewportPadding, maxLeft)
  const top = clamp(position.point.top, viewportPadding, maxTop)

  popoverStyle.value = {
    left: `${left}px`,
    top: `${top}px`,
    maxWidth: `calc(100vw - ${viewportPadding * 2}px)`,
    ...(toCssLength(props.width) ? {} : { minWidth: `${triggerRect.width}px` }),
  }
  popoverArrowStyle.value = popoverArrowPosition({
    placement: position.placement,
    triggerRect,
    panelWidth,
    panelHeight,
    left,
    top,
  })
}

function choosePopoverPosition(options: {
  placement: PopoverPlacement
  triggerRect: DOMRect
  panelWidth: number
  panelHeight: number
  offset: number
  viewportPadding: number
}): PopoverPosition {
  const placements = placementCandidates(options.placement)

  for (const placement of placements) {
    const point = popoverPoint(placement, options)
    if (isPointInViewport(point, options)) {
      return { placement, point }
    }
  }

  return {
    placement: options.placement,
    point: popoverPoint(options.placement, options),
  }
}

function placementCandidates(placement: PopoverPlacement): PopoverPlacement[] {
  const all: PopoverPlacement[] = [
    'bottom-end',
    'bottom-start',
    'bottom',
    'top-end',
    'top-start',
    'top',
    'right',
    'left',
  ]
  const opposite: Record<PopoverPlacement, PopoverPlacement> = {
    'top': 'bottom',
    'top-start': 'bottom-start',
    'top-end': 'bottom-end',
    'right': 'left',
    'bottom': 'top',
    'bottom-start': 'top-start',
    'bottom-end': 'top-end',
    'left': 'right',
  }

  return [
    placement,
    opposite[placement],
    ...all.filter(item => item !== placement && item !== opposite[placement]),
  ]
}

function popoverPoint(
  placement: PopoverPlacement,
  options: {
    triggerRect: DOMRect
    panelWidth: number
    panelHeight: number
    offset: number
  },
): PopoverPoint {
  const { triggerRect, panelWidth, panelHeight, offset } = options
  const centerLeft = triggerRect.left + triggerRect.width / 2 - panelWidth / 2
  const centerTop = triggerRect.top + triggerRect.height / 2 - panelHeight / 2

  const points: Record<PopoverPlacement, PopoverPoint> = {
    'top': { left: centerLeft, top: triggerRect.top - panelHeight - offset },
    'top-start': { left: triggerRect.left, top: triggerRect.top - panelHeight - offset },
    'top-end': {
      left: triggerRect.right - panelWidth,
      top: triggerRect.top - panelHeight - offset,
    },
    'right': { left: triggerRect.right + offset, top: centerTop },
    'bottom': { left: centerLeft, top: triggerRect.bottom + offset },
    'bottom-start': { left: triggerRect.left, top: triggerRect.bottom + offset },
    'bottom-end': { left: triggerRect.right - panelWidth, top: triggerRect.bottom + offset },
    'left': { left: triggerRect.left - panelWidth - offset, top: centerTop },
  }

  return points[placement]
}

function isPointInViewport(
  point: PopoverPoint,
  options: {
    panelWidth: number
    panelHeight: number
    viewportPadding: number
  },
) {
  const { panelWidth, panelHeight, viewportPadding } = options

  return (
    point.left >= viewportPadding
    && point.top >= viewportPadding
    && point.left + panelWidth <= window.innerWidth - viewportPadding
    && point.top + panelHeight <= window.innerHeight - viewportPadding
  )
}

function popoverArrowPosition(options: {
  placement: PopoverPlacement
  triggerRect: DOMRect
  panelWidth: number
  panelHeight: number
  left: number
  top: number
}): CSSProperties {
  const arrowSize = 8
  const arrowHalf = arrowSize / 2
  const arrowPadding = 12
  const { placement, triggerRect, panelWidth, panelHeight, left, top } = options
  const side = placement.split('-')[0]
  const centerX = triggerRect.left + triggerRect.width / 2 - left
  const centerY = triggerRect.top + triggerRect.height / 2 - top

  if (side === 'top' || side === 'bottom') {
    const arrowLeft = clamp(centerX - arrowHalf, arrowPadding, panelWidth - arrowPadding)

    return {
      left: `${arrowLeft}px`,
      top: side === 'bottom' ? `${-arrowHalf}px` : `${panelHeight - arrowHalf}px`,
    }
  }

  const arrowTop = clamp(centerY - arrowHalf, arrowPadding, panelHeight - arrowPadding)

  return {
    left: side === 'right' ? `${-arrowHalf}px` : `${panelWidth - arrowHalf}px`,
    top: `${arrowTop}px`,
  }
}

const updatePopoverPositionThrottled = useThrottleFn(updatePopoverPosition, 32, true)

async function openPopover() {
  if (props.disabled || open.value)
    return

  clearHoverCloseTimer()
  open.value = true
  await nextTick()
  updatePopoverPosition()
}

function closePopover() {
  clearHoverCloseTimer()
  open.value = false
}

function togglePopover() {
  if (props.trigger !== 'click') {
    return
  }

  if (open.value) {
    closePopover()
    return
  }

  void openPopover()
}

function clearHoverCloseTimer() {
  if (hoverCloseTimer.value === undefined) {
    return
  }

  window.clearTimeout(hoverCloseTimer.value)
  hoverCloseTimer.value = undefined
}

function handleHoverEnter() {
  if (props.trigger !== 'hover') {
    return
  }

  clearHoverCloseTimer()
  void openPopover()
}

function handleHoverLeave() {
  if (props.trigger !== 'hover') {
    return
  }

  clearHoverCloseTimer()
  hoverCloseTimer.value = window.setTimeout(closePopover, 90)
}

function handleTriggerKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    closePopover()
  }
}

whenever(open, async () => {
  await nextTick()
  updatePopoverPosition()
})
watch(
  () => props.anchorElement,
  async () => {
    if (!open.value)
      return
    await nextTick()
    updatePopoverPosition()
  },
)

onClickOutside(rootRef, closePopover, { ignore: [popoverRef] })
useEventListener(viewportTarget, 'keydown', (event) => {
  if (event instanceof KeyboardEvent && event.key === 'Escape') {
    closePopover()
  }
})
useEventListener(viewportTarget, 'resize', updatePopoverPositionThrottled)
useEventListener(viewportTarget, 'scroll', updatePopoverPositionThrottled, { capture: true })
onBeforeUnmount(clearHoverCloseTimer)
</script>

<template>
  <div ref="rootRef" class="relative inline-block overflow-visible" v-bind="attrs">
    <div
      ref="triggerRef"
      class="inline-flex"
      :class="props.triggerClass"
      @click.stop="togglePopover"
      @keydown="handleTriggerKeydown"
      @mouseenter="handleHoverEnter"
      @mouseleave="handleHoverLeave"
    >
      <slot name="trigger" :open="open" :close="closePopover" :toggle="togglePopover" />
    </div>

    <Teleport to="body">
      <Transition
        enter-active-class="transition-[opacity,transform] duration-150 ease-out motion-reduce:transition-none"
        enter-from-class="-translate-y-1 opacity-0"
        enter-to-class="translate-y-0 opacity-100"
        leave-active-class="transition-opacity duration-150 ease-in motion-reduce:transition-none"
        leave-from-class="opacity-100"
        leave-to-class="opacity-0"
      >
        <div
          v-if="open"
          ref="popoverRef"
          :class="popoverClasses"
          :style="[sizeStyle, popoverStyle]"
          @mouseenter="handleHoverEnter"
          @mouseleave="handleHoverLeave"
        >
          <span
            class="pointer-events-none absolute size-2 rotate-45 bg-inherit"
            :class="
              props.animatePosition
                ? 'transition-[left,top] duration-150 ease-out motion-reduce:transition-none'
                : undefined
            "
            :style="popoverArrowStyle"
          />
          <slot :open="open" :close="closePopover" :toggle="togglePopover" />
        </div>
      </Transition>
    </Teleport>
  </div>
</template>
