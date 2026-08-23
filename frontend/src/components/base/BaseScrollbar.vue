<script setup lang="ts">
import { useEventListener, useResizeObserver, useTimeoutFn } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { computed, nextTick, onBeforeUnmount, onMounted, shallowRef, useTemplateRef } from 'vue'

const props = withDefaults(
  defineProps<{
    maxHeight?: string
    height?: string
    horizontal?: boolean
    vertical?: boolean
    alwaysVisible?: boolean
  }>(),
  {
    maxHeight: undefined,
    height: undefined,
    horizontal: false,
    vertical: true,
    alwaysVisible: false,
  },
)

const emit = defineEmits<{
  scroll: [payload: { scrollTop: number, scrollLeft: number }]
}>()

const overflowTolerance = 1

const rootRef = useTemplateRef<HTMLDivElement>('root')
const wrapRef = useTemplateRef<HTMLDivElement>('wrap')
const viewRef = useTemplateRef<HTMLElement>('view')
const verticalTrackRef = useTemplateRef<HTMLDivElement>('verticalTrack')
const horizontalTrackRef = useTemplateRef<HTMLDivElement>('horizontalTrack')
const thumbHeight = shallowRef(0)
const thumbTop = shallowRef(0)
const horizontalThumbWidth = shallowRef(0)
const horizontalThumbLeft = shallowRef(0)
const visible = shallowRef(false)
const verticalTrackHovering = shallowRef(false)
const horizontalTrackHovering = shallowRef(false)
const dragging = shallowRef(false)
const horizontalDragging = shallowRef(false)
const dragDocument = computed(() =>
  dragging.value || horizontalDragging.value ? document : null,
)

interface AxisMetrics {
  scrollRange: number
  thumbRange: number
}

const verticalMetrics: AxisMetrics = { scrollRange: 0, thumbRange: 0 }
const horizontalMetrics: AxisMetrics = { scrollRange: 0, thumbRange: 0 }

let dragStartY = 0
let dragStartScrollTop = 0
let horizontalDragStartX = 0
let horizontalDragStartScrollLeft = 0
let scrollTopPosition = 0
let scrollLeftPosition = 0
let scrollFrameId: number | undefined
const { start: startHideTimer, stop: stopHideTimer } = useTimeoutFn(hideScrollbar, 1600, {
  immediate: false,
})

const canScrollY = computed(() => thumbHeight.value > 0)
const canScrollX = computed(() => horizontalThumbWidth.value > 0)
const verticalScrollbarVisible = computed(
  () => props.alwaysVisible || dragging.value || verticalTrackHovering.value || visible.value,
)
const horizontalScrollbarVisible = computed(
  () => props.alwaysVisible || horizontalDragging.value || horizontalTrackHovering.value || visible.value,
)
const thumbStyle = computed(() => ({
  height: `${thumbHeight.value}px`,
  transform: `translateY(${thumbTop.value}px)`,
}))
const horizontalThumbStyle = computed(() => ({
  width: `${horizontalThumbWidth.value}px`,
  transform: `translateX(${horizontalThumbLeft.value}px)`,
}))
const rootClasses = computed(() => [
  'relative min-h-0 overflow-hidden',
  props.maxHeight || props.height ? undefined : 'h-full',
])
const wrapClasses = computed(() => [
  'base-scrollbar-wrap min-h-0 max-h-[inherit] overflow-auto outline-none',
  props.maxHeight ? undefined : 'h-full',
])
const verticalTrackClass = computed(() =>
  props.horizontal && canScrollX.value ? 'bottom-1.5' : 'bottom-0',
)
const horizontalTrackClass = computed(() =>
  props.vertical && canScrollY.value ? 'right-1.5' : 'right-0',
)

function showScrollbar() {
  visible.value = true
}

function clearHideTimer() {
  stopHideTimer()
}

function scheduleHideScrollbar() {
  clearHideTimer()
  if (!dragging.value
    && !horizontalDragging.value
    && !verticalTrackHovering.value
    && !horizontalTrackHovering.value) {
    startHideTimer()
  }
}

function hideScrollbar() {
  clearHideTimer()
  visible.value = false
}

function activateScrollbar() {
  showScrollbar()
  scheduleHideScrollbar()
}

function handleRootMouseEnter() {
  activateScrollbar()
}

function handleRootMouseLeave() {
  hideScrollbar()
}

type ScrollbarAxis = 'vertical' | 'horizontal'

function setTrackHovering(axis: ScrollbarAxis, hovering: boolean) {
  if (axis === 'vertical') {
    verticalTrackHovering.value = hovering
    return
  }

  horizontalTrackHovering.value = hovering
}

function handleTrackMouseEnter(axis: ScrollbarAxis) {
  setTrackHovering(axis, true)
  showScrollbar()
  clearHideTimer()
}

function handleTrackMouseLeave(axis: ScrollbarAxis) {
  setTrackHovering(axis, false)
  scheduleHideScrollbar()
}

function update() {
  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  measureVerticalScrollbar(wrap)
  measureHorizontalScrollbar(wrap)
  syncScrollbarPosition(wrap, true)
}

function measureVerticalScrollbar(wrap: HTMLElement) {
  if (!props.vertical) {
    verticalMetrics.scrollRange = 0
    verticalMetrics.thumbRange = 0
    thumbHeight.value = 0
    return
  }

  const clientHeight = wrap.clientHeight
  const scrollHeight = wrap.scrollHeight
  const scrollRange = clamp(scrollHeight - clientHeight, 0, Number.POSITIVE_INFINITY)
  const availableTrackHeight = trackLength(
    verticalTrackRef.value,
    clientHeight,
    'top',
    'bottom',
  )
  if (scrollRange <= overflowTolerance || availableTrackHeight <= 0) {
    wrap.scrollTop = 0
    verticalMetrics.scrollRange = 0
    verticalMetrics.thumbRange = 0
    thumbHeight.value = 0
    return
  }

  const ratio = clientHeight / scrollHeight
  const nextThumbHeight = clamp(availableTrackHeight * ratio, 32, availableTrackHeight)
  verticalMetrics.scrollRange = scrollRange
  verticalMetrics.thumbRange = availableTrackHeight - nextThumbHeight
  thumbHeight.value = nextThumbHeight
}

function measureHorizontalScrollbar(wrap: HTMLElement) {
  if (!props.horizontal) {
    horizontalMetrics.scrollRange = 0
    horizontalMetrics.thumbRange = 0
    horizontalThumbWidth.value = 0
    return
  }

  const clientWidth = wrap.clientWidth
  const scrollWidth = wrap.scrollWidth
  const scrollRange = clamp(scrollWidth - clientWidth, 0, Number.POSITIVE_INFINITY)
  const availableTrackWidth = trackLength(
    horizontalTrackRef.value,
    clientWidth,
    'left',
    'right',
  )
  if (scrollRange <= overflowTolerance || availableTrackWidth <= 0) {
    wrap.scrollLeft = 0
    horizontalMetrics.scrollRange = 0
    horizontalMetrics.thumbRange = 0
    horizontalThumbWidth.value = 0
    return
  }

  const ratio = clientWidth / scrollWidth
  const nextThumbWidth = clamp(availableTrackWidth * ratio, 32, availableTrackWidth)
  horizontalMetrics.scrollRange = scrollRange
  horizontalMetrics.thumbRange = availableTrackWidth - nextThumbWidth
  horizontalThumbWidth.value = nextThumbWidth
}

function trackLength(
  track: HTMLElement | null,
  viewportLength: number,
  startProperty: 'top' | 'left',
  endProperty: 'bottom' | 'right',
) {
  if (!track)
    return clamp(viewportLength - 8, 0, Number.POSITIVE_INFINITY)

  const style = getComputedStyle(track)
  const start = Number.parseFloat(style[startProperty])
  const end = Number.parseFloat(style[endProperty])
  const inset = (Number.isFinite(start) ? start : 4) + (Number.isFinite(end) ? end : 4)
  const root = rootRef.value
  const containerLength = root
    ? startProperty === 'top'
      ? root.clientHeight
      : root.clientWidth
    : viewportLength
  return clamp(containerLength - inset, 0, Number.POSITIVE_INFINITY)
}

function updateVerticalThumbPosition(scrollTop: number) {
  const { scrollRange, thumbRange } = verticalMetrics
  thumbTop.value
    = scrollRange > 0 ? (clamp(scrollTop, 0, scrollRange) / scrollRange) * thumbRange : 0
}

function updateHorizontalThumbPosition(scrollLeft: number) {
  const { scrollRange, thumbRange } = horizontalMetrics
  horizontalThumbLeft.value
    = scrollRange > 0 ? (clamp(scrollLeft, 0, scrollRange) / scrollRange) * thumbRange : 0
}

function syncScrollbarPosition(wrap: HTMLElement, force = false) {
  const scrollTop = wrap.scrollTop
  const scrollLeft = wrap.scrollLeft

  if (force || scrollTop !== scrollTopPosition) {
    updateVerticalThumbPosition(scrollTop)
  }
  if (force || scrollLeft !== scrollLeftPosition) {
    updateHorizontalThumbPosition(scrollLeft)
  }

  scrollTopPosition = scrollTop
  scrollLeftPosition = scrollLeft
}

async function scrollToTop() {
  const wrap = wrapRef.value
  if (wrap) {
    wrap.scrollTop = 0
    wrap.scrollLeft = 0
  }
  dragging.value = false
  horizontalDragging.value = false
  visible.value = false
  clearHideTimer()
  await nextTick()
  update()
}

async function scrollToBottom() {
  const wrap = wrapRef.value
  if (wrap) {
    wrap.scrollTop = wrap.scrollHeight
  }
  await nextTick()
  update()
}

function flushScrollFrame() {
  scrollFrameId = undefined
  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  syncScrollbarPosition(wrap)
  activateScrollbar()
  emit('scroll', {
    scrollTop: wrap.scrollTop,
    scrollLeft: wrap.scrollLeft,
  })
}

function handleScroll() {
  if (scrollFrameId !== undefined) {
    return
  }

  scrollFrameId = requestAnimationFrame(flushScrollFrame)
}

function handleTrackPointerDown(event: PointerEvent) {
  if (event.target !== event.currentTarget) {
    return
  }

  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  update()
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
  activateScrollbar()
  const nextThumbTop = event.clientY - rect.top - thumbHeight.value / 2
  const { scrollRange, thumbRange } = verticalMetrics
  wrap.scrollTop
    = thumbRange > 0 ? (clamp(nextThumbTop, 0, thumbRange) / thumbRange) * scrollRange : 0
}

function handleHorizontalTrackPointerDown(event: PointerEvent) {
  if (event.target !== event.currentTarget) {
    return
  }

  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  update()
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
  activateScrollbar()
  const nextThumbLeft = event.clientX - rect.left - horizontalThumbWidth.value / 2
  const { scrollRange, thumbRange } = horizontalMetrics
  wrap.scrollLeft
    = thumbRange > 0 ? (clamp(nextThumbLeft, 0, thumbRange) / thumbRange) * scrollRange : 0
}

function handleThumbPointerDown(event: PointerEvent) {
  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  event.preventDefault()
  update()
  dragging.value = true
  visible.value = true
  clearHideTimer()
  dragStartY = event.clientY
  dragStartScrollTop = wrap.scrollTop
}

function handleHorizontalThumbPointerDown(event: PointerEvent) {
  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  event.preventDefault()
  update()
  horizontalDragging.value = true
  visible.value = true
  clearHideTimer()
  horizontalDragStartX = event.clientX
  horizontalDragStartScrollLeft = wrap.scrollLeft
}

function handleThumbPointerMove(event: PointerEvent) {
  if (!dragging.value) {
    return
  }

  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  const { scrollRange, thumbRange } = verticalMetrics
  if (thumbRange <= 0) {
    return
  }

  wrap.scrollTop = dragStartScrollTop + ((event.clientY - dragStartY) / thumbRange) * scrollRange
}

function handleHorizontalThumbPointerMove(event: PointerEvent) {
  if (!horizontalDragging.value) {
    return
  }

  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  const { scrollRange, thumbRange } = horizontalMetrics
  if (thumbRange <= 0) {
    return
  }

  wrap.scrollLeft
    = horizontalDragStartScrollLeft
      + ((event.clientX - horizontalDragStartX) / thumbRange) * scrollRange
}

function handleThumbPointerUp() {
  if (!dragging.value) {
    return
  }

  dragging.value = false
  activateScrollbar()
}

function handleHorizontalThumbPointerUp() {
  if (!horizontalDragging.value) {
    return
  }

  horizontalDragging.value = false
  activateScrollbar()
}

function handleDocumentPointerMove(event: PointerEvent) {
  handleThumbPointerMove(event)
  handleHorizontalThumbPointerMove(event)
}

function handleDocumentPointerUp() {
  handleThumbPointerUp()
  handleHorizontalThumbPointerUp()
}

onMounted(async () => {
  await nextTick()
  update()
})

onBeforeUnmount(() => {
  if (scrollFrameId !== undefined) {
    cancelAnimationFrame(scrollFrameId)
  }
})

useResizeObserver([wrapRef, viewRef], update)
useEventListener(wrapRef, 'scroll', handleScroll, { passive: true })
useEventListener(dragDocument, 'pointermove', handleDocumentPointerMove)
useEventListener(dragDocument, ['pointerup', 'pointercancel'], handleDocumentPointerUp)
useEventListener(rootRef, 'mouseenter', handleRootMouseEnter)
useEventListener(rootRef, 'mousemove', activateScrollbar, { passive: true })
useEventListener(rootRef, 'mouseleave', handleRootMouseLeave)
useEventListener(verticalTrackRef, 'mouseenter', () => handleTrackMouseEnter('vertical'))
useEventListener(verticalTrackRef, 'mouseleave', () => handleTrackMouseLeave('vertical'))
useEventListener(verticalTrackRef, 'pointerdown', handleTrackPointerDown)
useEventListener(horizontalTrackRef, 'mouseenter', () => handleTrackMouseEnter('horizontal'))
useEventListener(horizontalTrackRef, 'mouseleave', () => handleTrackMouseLeave('horizontal'))
useEventListener(horizontalTrackRef, 'pointerdown', handleHorizontalTrackPointerDown)

defineExpose({
  update,
  scrollToTop,
  scrollToBottom,
  wrapRef,
})
</script>

<template>
  <div
    ref="root"
    :class="rootClasses"
    :style="{ maxHeight, height }"
  >
    <div ref="wrap" :class="wrapClasses" tabindex="0">
      <div ref="view">
        <slot />
      </div>
    </div>

    <div
      v-show="canScrollY"
      ref="verticalTrack"
      aria-hidden="true"
      class="base-scrollbar-track-y absolute right-0 bottom-0 z-40 flex w-3 justify-end transition-opacity duration-200"
      :class="[
        verticalTrackClass,
        verticalScrollbarVisible ? 'opacity-100' : 'opacity-0',
      ]"
    >
      <div
        class="w-1.5 rounded-full bg-(--cp-scrollbar-thumb-bg) transition-colors duration-200 hover:bg-(--cp-scrollbar-thumb-hover-bg)"
        :class="dragging ? 'bg-(--cp-scrollbar-thumb-hover-bg)' : ''"
        :style="thumbStyle"
        @pointerdown="handleThumbPointerDown"
      />
    </div>

    <div
      v-show="canScrollX"
      ref="horizontalTrack"
      aria-hidden="true"
      class="base-scrollbar-track-x absolute bottom-0 left-0 z-40 flex h-3 items-end transition-opacity duration-200"
      :class="[
        horizontalTrackClass,
        horizontalScrollbarVisible ? 'opacity-100' : 'opacity-0',
      ]"
    >
      <div
        class="h-1.5 rounded-full bg-(--cp-scrollbar-thumb-bg) transition-colors duration-200 hover:bg-(--cp-scrollbar-thumb-hover-bg)"
        :class="horizontalDragging ? 'bg-(--cp-scrollbar-thumb-hover-bg)' : ''"
        :style="horizontalThumbStyle"
        @pointerdown="handleHorizontalThumbPointerDown"
      />
    </div>
  </div>
</template>

<style scoped>
.base-scrollbar-wrap {
  scrollbar-width: none;
}

.base-scrollbar-wrap::-webkit-scrollbar {
  display: none;
  width: 0;
  height: 0;
}

.base-scrollbar-track-y {
  top: var(--cp-scrollbar-track-inset-block-start, 0.25rem);
}
</style>
