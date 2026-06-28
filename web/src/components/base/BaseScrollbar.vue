<script setup lang="ts">
import { useEventListener, useResizeObserver, useScroll, useTimeoutFn } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { computed, nextTick, onMounted, shallowRef, useTemplateRef } from 'vue'

const props = withDefaults(
  defineProps<{
    viewClass?: string
    maxHeight?: string
    forceVisible?: boolean
  }>(),
  {
    viewClass: '',
    maxHeight: undefined,
    forceVisible: false,
  },
)

const emit = defineEmits<{
  scroll: [payload: { scrollTop: number; scrollLeft: number }]
}>()

const wrapRef = useTemplateRef<HTMLDivElement>('wrap')
const viewRef = useTemplateRef<HTMLElement>('view')
const thumbHeight = shallowRef(0)
const thumbTop = shallowRef(0)
const visible = shallowRef(false)
const dragging = shallowRef(false)

let dragStartY = 0
let dragStartScrollTop = 0
const { start: startHideTimer, stop: stopHideTimer } = useTimeoutFn(hideScrollbar, 900, {
  immediate: false,
})

const canScrollY = computed(() => thumbHeight.value > 0)
const scrollbarVisible = computed(() => props.forceVisible || dragging.value || visible.value)
const thumbStyle = computed(() => ({
  height: `${thumbHeight.value}px`,
  transform: `translateY(${thumbTop.value}px)`,
}))
const rootClasses = computed(() => [
  'relative min-h-0 overflow-hidden',
  props.maxHeight ? undefined : 'h-full',
])

function trackHeight(wrap: HTMLElement) {
  return clamp(wrap.clientHeight - 8, 0, Number.POSITIVE_INFINITY)
}

function maxScrollTop(wrap: HTMLElement) {
  return clamp(wrap.scrollHeight - wrap.clientHeight, 0, Number.POSITIVE_INFINITY)
}

function maxThumbTop(wrap: HTMLElement) {
  return clamp(trackHeight(wrap) - thumbHeight.value, 0, Number.POSITIVE_INFINITY)
}

function showScrollbar() {
  visible.value = true
}

function clearHideTimer() {
  stopHideTimer()
}

function scheduleHideScrollbar() {
  clearHideTimer()
  if (props.forceVisible || dragging.value) {
    return
  }

  startHideTimer()
}

function hideScrollbar() {
  clearHideTimer()
  if (!props.forceVisible && !dragging.value) {
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

  const scrollRange = maxScrollTop(wrap)
  const availableTrackHeight = trackHeight(wrap)
  if (scrollRange <= 0 || availableTrackHeight <= 0) {
    thumbHeight.value = 0
    thumbTop.value = 0
    visible.value = false
    clearHideTimer()
    return
  }

  const ratio = wrap.clientHeight / wrap.scrollHeight
  thumbHeight.value = clamp(availableTrackHeight * ratio, 32, availableTrackHeight)
  thumbTop.value = (wrap.scrollTop / scrollRange) * maxThumbTop(wrap)
}

async function scrollToTop() {
  const wrap = wrapRef.value
  if (wrap) {
    wrap.scrollTop = 0
    wrap.scrollLeft = 0
  }
  dragging.value = false
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

function handleThumbPointerUp() {
  if (!dragging.value) {
    return
  }

  dragging.value = false
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

defineExpose({
  update,
  scrollToTop,
  wrapRef,
})
</script>

<template>
  <div :class="rootClasses" :style="{ maxHeight }">
    <div
      ref="wrap"
      class="h-full min-h-0 overflow-auto max-h-[inherit] [-ms-overflow-style:none] scrollbar-none [&::-webkit-scrollbar]:h-0 [&::-webkit-scrollbar]:w-0 [&::-webkit-scrollbar]:bg-transparent"
      @mouseenter="activateScrollbar"
      @mouseleave="hideScrollbar"
    >
      <div ref="view" :class="viewClass">
        <slot />
      </div>
    </div>

    <div
      v-show="canScrollY"
      class="absolute top-1 right-1 bottom-1 z-10 w-1.5 rounded-full transition-opacity duration-200"
      :class="scrollbarVisible ? 'opacity-100' : 'opacity-0'"
      @pointerdown="handleTrackPointerDown"
    >
      <div
        class="w-full rounded-full bg-(--cp-scrollbar-thumb) transition-colors duration-200 hover:bg-(--cp-scrollbar-thumb-hover)"
        :class="dragging ? 'bg-(--cp-scrollbar-thumb-hover)' : ''"
        :style="thumbStyle"
        @pointerdown="handleThumbPointerDown"
      />
    </div>
  </div>
</template>
