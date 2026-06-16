<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, shallowRef, useTemplateRef } from 'vue'

const props = withDefaults(defineProps<{
  tag?: string
  always?: boolean
  minThumbSize?: number
  wrapClass?: string
  viewClass?: string
}>(), {
  tag: 'div',
  always: false,
  minThumbSize: 32,
  wrapClass: '',
  viewClass: '',
})

const emit = defineEmits<{
  scroll: [payload: { scrollTop: number, scrollLeft: number }]
}>()

const wrapRef = useTemplateRef<HTMLDivElement>('wrap')
const viewRef = useTemplateRef<HTMLElement>('view')
const thumbHeight = shallowRef(0)
const thumbTop = shallowRef(0)
const visible = shallowRef(false)
const dragging = shallowRef(false)

let resizeObserver: ResizeObserver | undefined
let hideTimer: number | undefined
let dragStartY = 0
let dragStartScrollTop = 0

const canScrollY = computed(() => thumbHeight.value > 0)
const scrollbarVisible = computed(() => props.always || dragging.value || visible.value)
const thumbStyle = computed(() => ({
  height: `${thumbHeight.value}px`,
  transform: `translateY(${thumbTop.value}px)`,
}))

function trackHeight(wrap: HTMLElement) {
  return Math.max(wrap.clientHeight - 8, 0)
}

function maxScrollTop(wrap: HTMLElement) {
  return Math.max(wrap.scrollHeight - wrap.clientHeight, 0)
}

function maxThumbTop(wrap: HTMLElement) {
  return Math.max(trackHeight(wrap) - thumbHeight.value, 0)
}

function showScrollbar() {
  visible.value = true
  if (hideTimer) {
    window.clearTimeout(hideTimer)
  }
  if (!props.always && !dragging.value) {
    hideTimer = window.setTimeout(() => {
      visible.value = false
    }, 900)
  }
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
    return
  }

  const ratio = wrap.clientHeight / wrap.scrollHeight
  thumbHeight.value = Math.min(
    availableTrackHeight,
    Math.max(availableTrackHeight * ratio, props.minThumbSize),
  )
  thumbTop.value = (wrap.scrollTop / scrollRange) * maxThumbTop(wrap)
}

function handleScroll() {
  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  update()
  showScrollbar()
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
  const nextThumbTop = event.clientY - rect.top - thumbHeight.value / 2
  const scrollRange = maxScrollTop(wrap)
  const thumbRange = maxThumbTop(wrap)
  wrap.scrollTop = thumbRange > 0
    ? (Math.max(0, Math.min(nextThumbTop, thumbRange)) / thumbRange) * scrollRange
    : 0
}

function handleThumbPointerDown(event: PointerEvent) {
  const wrap = wrapRef.value
  if (!wrap) {
    return
  }

  event.preventDefault()
  dragging.value = true
  visible.value = true
  dragStartY = event.clientY
  dragStartScrollTop = wrap.scrollTop
  document.addEventListener('pointermove', handleThumbPointerMove)
  document.addEventListener('pointerup', handleThumbPointerUp, { once: true })
}

function handleThumbPointerMove(event: PointerEvent) {
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
  dragging.value = false
  document.removeEventListener('pointermove', handleThumbPointerMove)
  showScrollbar()
}

function scrollTo(options: ScrollToOptions): void
function scrollTo(x: number, y?: number): void
function scrollTo(arg1: ScrollToOptions | number, arg2?: number) {
  if (typeof arg1 === 'number') {
    wrapRef.value?.scrollTo(arg1, arg2 ?? 0)
    return
  }
  wrapRef.value?.scrollTo(arg1)
}

function setScrollTop(value: number) {
  if (wrapRef.value) {
    wrapRef.value.scrollTop = value
  }
}

onMounted(async () => {
  await nextTick()
  update()
  resizeObserver = new ResizeObserver(update)
  if (wrapRef.value) {
    resizeObserver.observe(wrapRef.value)
  }
  if (viewRef.value) {
    resizeObserver.observe(viewRef.value)
  }
})

onBeforeUnmount(() => {
  resizeObserver?.disconnect()
  if (hideTimer) {
    window.clearTimeout(hideTimer)
  }
  document.removeEventListener('pointermove', handleThumbPointerMove)
})

defineExpose({
  update,
  scrollTo,
  setScrollTop,
  wrapRef,
})
</script>

<template>
  <div class="relative h-full min-h-0 overflow-hidden">
    <div
      ref="wrap"
      class="base-scrollbar-wrap h-full min-h-0 overflow-auto"
      :class="wrapClass"
      @mouseenter="showScrollbar"
      @scroll="handleScroll"
    >
      <component :is="tag" ref="view" :class="viewClass">
        <slot />
      </component>
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

<style scoped>
.base-scrollbar-wrap {
  scrollbar-width: none;
}

.base-scrollbar-wrap::-webkit-scrollbar {
  width: 0;
  height: 0;
}
</style>
