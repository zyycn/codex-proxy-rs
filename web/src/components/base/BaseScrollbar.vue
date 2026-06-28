<script setup lang="ts">
import { useEventListener, useResizeObserver, useScroll, useTimeoutFn } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { computed, nextTick, onMounted, shallowRef, useTemplateRef } from 'vue'

const props = withDefaults(
  defineProps<{
    viewClass?: string
    maxHeight?: string
    forceVisible?: boolean
    horizontal?: boolean
    vertical?: boolean
  }>(),
  {
    viewClass: '',
    maxHeight: undefined,
    forceVisible: false,
    horizontal: false,
    vertical: true,
  },
)

const emit = defineEmits<{
  scroll: [payload: { scrollTop: number; scrollLeft: number }]
}>()

const wrapRef = useTemplateRef<HTMLDivElement>('wrap')
const viewRef = useTemplateRef<HTMLElement>('view')
const thumbHeight = shallowRef(0)
const thumbTop = shallowRef(0)
const horizontalThumbWidth = shallowRef(0)
const horizontalThumbLeft = shallowRef(0)
const visible = shallowRef(false)
const dragging = shallowRef(false)
const horizontalDragging = shallowRef(false)

let dragStartY = 0
let dragStartScrollTop = 0
let horizontalDragStartX = 0
let horizontalDragStartScrollLeft = 0
const { start: startHideTimer, stop: stopHideTimer } = useTimeoutFn(hideScrollbar, 900, {
  immediate: false,
})

const canScrollY = computed(() => thumbHeight.value > 0)
const canScrollX = computed(() => horizontalThumbWidth.value > 0)
const scrollbarVisible = computed(
  () => props.forceVisible || dragging.value || horizontalDragging.value || visible.value,
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
  props.maxHeight ? undefined : 'h-full',
])
const wrapClasses = computed(() => [
  'min-h-0 overflow-auto max-h-[inherit] [-ms-overflow-style:none] scrollbar-none [&::-webkit-scrollbar]:h-0 [&::-webkit-scrollbar]:w-0 [&::-webkit-scrollbar]:bg-transparent',
  props.maxHeight ? undefined : 'h-full',
])
const verticalTrackClass = computed(() =>
  props.horizontal && canScrollX.value ? 'bottom-3' : 'bottom-1',
)
const horizontalTrackClass = computed(() =>
  props.vertical && canScrollY.value ? 'right-3' : 'right-1',
)

function trackHeight(wrap: HTMLElement) {
  return clamp(wrap.clientHeight - 8, 0, Number.POSITIVE_INFINITY)
}

function maxScrollTop(wrap: HTMLElement) {
  return clamp(wrap.scrollHeight - wrap.clientHeight, 0, Number.POSITIVE_INFINITY)
}

function trackWidth(wrap: HTMLElement) {
  return clamp(wrap.clientWidth - 8, 0, Number.POSITIVE_INFINITY)
}

function maxScrollLeft(wrap: HTMLElement) {
  return clamp(wrap.scrollWidth - wrap.clientWidth, 0, Number.POSITIVE_INFINITY)
}

function maxThumbTop(wrap: HTMLElement) {
  return clamp(trackHeight(wrap) - thumbHeight.value, 0, Number.POSITIVE_INFINITY)
}

function maxHorizontalThumbLeft(wrap: HTMLElement) {
  return clamp(trackWidth(wrap) - horizontalThumbWidth.value, 0, Number.POSITIVE_INFINITY)
}

function showScrollbar() {
  visible.value = true
}

function clearHideTimer() {
  stopHideTimer()
}

function scheduleHideScrollbar() {
  clearHideTimer()
  if (props.forceVisible || dragging.value || horizontalDragging.value) {
    return
  }

  startHideTimer()
}

function hideScrollbar() {
  clearHideTimer()
  if (!props.forceVisible && !dragging.value && !horizontalDragging.value) {
    visible.value = false
  }
}

function activateScrollbar() {
  showScrollbar()
  scheduleHideScrollbar()
}

function update() {
  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  updateVerticalScrollbar(wrap)
  updateHorizontalScrollbar(wrap)
}

function updateVerticalScrollbar(wrap: HTMLElement) {
  if (!props.vertical) {
    thumbHeight.value = 0
    thumbTop.value = 0
    return
  }

  const scrollRange = maxScrollTop(wrap)
  const availableTrackHeight = trackHeight(wrap)
  if (scrollRange <= 0 || availableTrackHeight <= 0) {
    thumbHeight.value = 0
    thumbTop.value = 0
    return
  }
  const ratio = wrap.clientHeight / wrap.scrollHeight
  thumbHeight.value = clamp(availableTrackHeight * ratio, 32, availableTrackHeight)
  thumbTop.value = (wrap.scrollTop / scrollRange) * maxThumbTop(wrap)
}

function updateHorizontalScrollbar(wrap: HTMLElement) {
  if (!props.horizontal) {
    horizontalThumbWidth.value = 0
    horizontalThumbLeft.value = 0
    return
  }

  const scrollRange = maxScrollLeft(wrap)
  const availableTrackWidth = trackWidth(wrap)
  if (scrollRange <= 0 || availableTrackWidth <= 0) {
    horizontalThumbWidth.value = 0
    horizontalThumbLeft.value = 0
    return
  }
  const ratio = wrap.clientWidth / wrap.scrollWidth
  horizontalThumbWidth.value = clamp(availableTrackWidth * ratio, 32, availableTrackWidth)
  horizontalThumbLeft.value = (wrap.scrollLeft / scrollRange) * maxHorizontalThumbLeft(wrap)
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

function handleScroll() {
  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  update()
  activateScrollbar()
  emit('scroll', {
    scrollTop: wrap.scrollTop,
    scrollLeft: wrap.scrollLeft,
  })
}

function handleTrackPointerDown(event: PointerEvent) {
  if (event.target !== event.currentTarget) {
    return
  }

  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
  activateScrollbar()
  const nextThumbTop = event.clientY - rect.top - thumbHeight.value / 2
  const scrollRange = maxScrollTop(wrap)
  const thumbRange = maxThumbTop(wrap)
  wrap.scrollTop =
    thumbRange > 0 ? (clamp(nextThumbTop, 0, thumbRange) / thumbRange) * scrollRange : 0
}

function handleHorizontalTrackPointerDown(event: PointerEvent) {
  if (event.target !== event.currentTarget) {
    return
  }

  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
  activateScrollbar()
  const nextThumbLeft = event.clientX - rect.left - horizontalThumbWidth.value / 2
  const scrollRange = maxScrollLeft(wrap)
  const thumbRange = maxHorizontalThumbLeft(wrap)
  wrap.scrollLeft =
    thumbRange > 0 ? (clamp(nextThumbLeft, 0, thumbRange) / thumbRange) * scrollRange : 0
}

function handleThumbPointerDown(event: PointerEvent) {
  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  event.preventDefault()
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

  const thumbRange = maxThumbTop(wrap)
  if (thumbRange <= 0) {
    return
  }

  const scrollRange = maxScrollTop(wrap)
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

  const thumbRange = maxHorizontalThumbLeft(wrap)
  if (thumbRange <= 0) {
    return
  }

  const scrollRange = maxScrollLeft(wrap)
  wrap.scrollLeft =
    horizontalDragStartScrollLeft +
    ((event.clientX - horizontalDragStartX) / thumbRange) * scrollRange
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

onMounted(async () => {
  await nextTick()
  update()
})

useResizeObserver([wrapRef, viewRef], update)
useScroll(wrapRef, { onScroll: handleScroll })
useEventListener(document, 'pointermove', handleThumbPointerMove)
useEventListener(document, 'pointerup', handleThumbPointerUp)
useEventListener(document, 'pointermove', handleHorizontalThumbPointerMove)
useEventListener(document, 'pointerup', handleHorizontalThumbPointerUp)

defineExpose({
  update,
  scrollToTop,
  wrapRef,
})
</script>

<template>
  <div
    :class="rootClasses"
    :style="{ maxHeight }"
    @mouseenter="activateScrollbar"
    @mouseleave="hideScrollbar"
  >
    <div ref="wrap" :class="wrapClasses">
      <div ref="view" :class="viewClass">
        <slot />
      </div>
    </div>

    <div
      v-show="canScrollY"
      class="absolute top-1 right-1 z-10 w-1.5 rounded-full transition-opacity duration-200"
      :class="[verticalTrackClass, scrollbarVisible ? 'opacity-100' : 'opacity-0']"
      @mouseenter="activateScrollbar"
      @pointerdown="handleTrackPointerDown"
    >
      <div
        class="w-full rounded-full bg-(--cp-scrollbar-thumb) transition-colors duration-200 hover:bg-(--cp-scrollbar-thumb-hover)"
        :class="dragging ? 'bg-(--cp-scrollbar-thumb-hover)' : ''"
        :style="thumbStyle"
        @pointerdown="handleThumbPointerDown"
      />
    </div>

    <div
      v-show="canScrollX"
      class="absolute bottom-1 left-1 z-10 h-1.5 rounded-full transition-opacity duration-200"
      :class="[horizontalTrackClass, scrollbarVisible ? 'opacity-100' : 'opacity-0']"
      @mouseenter="activateScrollbar"
      @pointerdown="handleHorizontalTrackPointerDown"
    >
      <div
        class="h-full rounded-full bg-(--cp-scrollbar-thumb) transition-colors duration-200 hover:bg-(--cp-scrollbar-thumb-hover)"
        :class="horizontalDragging ? 'bg-(--cp-scrollbar-thumb-hover)' : ''"
        :style="horizontalThumbStyle"
        @pointerdown="handleHorizontalThumbPointerDown"
      />
    </div>
  </div>
</template>
