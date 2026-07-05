<script setup lang="ts">
import { Menu } from '@lucide/vue'
import {
  useDraggable,
  useElementSize,
  useScreenSafeArea,
  useStorage,
  useWindowSize,
} from '@vueuse/core'
import {
  computed,
  nextTick,
  onMounted,
  onScopeDispose,
  shallowRef,
  useTemplateRef,
  watch,
} from 'vue'

import type { CSSProperties } from 'vue'

interface StoredPosition {
  initialized: boolean
  side: 'left' | 'right'
  x: number
  y: number
}

const emit = defineEmits<{
  open: []
}>()

const edgeGap = 12
const defaultButtonSize = 44
const dragClickThreshold = 6
const storageKey = 'codex-proxy:mobile-sidebar-toggle-position'

const toggleEl = useTemplateRef<HTMLElement>('toggleEl')
const { width: viewportWidth, height: viewportHeight } = useWindowSize({
  initialWidth: 390,
  initialHeight: 760,
  type: 'visual',
})
const { width: toggleWidth, height: toggleHeight } = useElementSize(toggleEl, {
  width: defaultButtonSize,
  height: defaultButtonSize,
})
const safeArea = useScreenSafeArea()
const storedPosition = useStorage<StoredPosition>(
  storageKey,
  {
    initialized: false,
    side: 'left',
    x: edgeGap,
    y: 136,
  },
  undefined,
  { mergeDefaults: true, shallow: true },
)
const dragStart = shallowRef({ x: edgeGap, y: 136 })
const pointerStart = shallowRef({ x: edgeGap, y: 136 })
const ignoreNextClick = shallowRef(false)
const positionReady = shallowRef(false)
let ignoreClickTimer: number | undefined

const bounds = computed(() => {
  const buttonWidth = positiveNumber(toggleWidth.value, defaultButtonSize)
  const buttonHeight = positiveNumber(toggleHeight.value, defaultButtonSize)
  const windowWidth = positiveNumber(viewportWidth.value, 390)
  const windowHeight = positiveNumber(viewportHeight.value, 760)
  const minX = parseCssPixel(safeArea.left.value) + edgeGap
  const maxX = Math.max(
    minX,
    windowWidth - parseCssPixel(safeArea.right.value) - edgeGap - buttonWidth,
  )
  const minY = parseCssPixel(safeArea.top.value) + edgeGap
  const maxY = Math.max(
    minY,
    windowHeight - parseCssPixel(safeArea.bottom.value) - edgeGap - buttonHeight,
  )

  return { minX, maxX, minY, maxY }
})

const { x, y, isDragging } = useDraggable(toggleEl, {
  initialValue: {
    x: storedPosition.value.x,
    y: storedPosition.value.y,
  },
  preventDefault: false,
  stopPropagation: false,
  onStart(position) {
    dragStart.value = { x: position.x, y: position.y }
  },
  onEnd(position) {
    applyPosition(snapPosition(position.x, position.y))
  },
})

const toggleStyle = computed<CSSProperties>(() => ({
  transform: `translate3d(${x.value}px, ${y.value}px, 0)`,
  touchAction: 'none',
}))

function handlePointerDown(event: PointerEvent) {
  pointerStart.value = { x: event.clientX, y: event.clientY }
  event.currentTarget instanceof HTMLElement &&
    event.currentTarget.setPointerCapture?.(event.pointerId)
}

function handlePointerUp(event: PointerEvent) {
  event.currentTarget instanceof HTMLElement &&
    event.currentTarget.releasePointerCapture?.(event.pointerId)

  const moved = Math.hypot(
    event.clientX - pointerStart.value.x,
    event.clientY - pointerStart.value.y,
  )
  ignoreNextClick.value = true
  window.clearTimeout(ignoreClickTimer)
  ignoreClickTimer = window.setTimeout(() => {
    ignoreNextClick.value = false
  }, 320)

  if (moved <= dragClickThreshold) {
    emit('open')
  }
}

function handleClick() {
  if (isDragging.value || ignoreNextClick.value) {
    ignoreNextClick.value = false
    return
  }

  emit('open')
}

function defaultPosition() {
  const targetY = Math.round(positiveNumber(viewportHeight.value, 760) * 0.36)
  return snapPosition(bounds.value.minX, targetY)
}

function restorePosition() {
  if (!storedPosition.value.initialized) {
    applyPosition(defaultPosition())
    return
  }

  const sideX = storedPosition.value.side === 'right' ? bounds.value.maxX : bounds.value.minX
  applyPosition({
    initialized: true,
    side: storedPosition.value.side,
    x: sideX,
    y: storedPosition.value.y,
  })
}

function snapPosition(rawX: number, rawY: number) {
  const { minX, maxX, minY, maxY } = bounds.value
  const side = rawX <= (minX + maxX) / 2 ? 'left' : 'right'

  return {
    initialized: true,
    side,
    x: side === 'left' ? minX : maxX,
    y: clamp(rawY, minY, maxY),
  } satisfies StoredPosition
}

function applyPosition(position: StoredPosition) {
  const { minX, maxX, minY, maxY } = bounds.value
  const next = {
    initialized: true,
    side: position.side,
    x: clamp(position.x, minX, maxX),
    y: clamp(position.y, minY, maxY),
  } satisfies StoredPosition

  x.value = next.x
  y.value = next.y
  storedPosition.value = next
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max)
}

function positiveNumber(value: number, fallback: number) {
  return Number.isFinite(value) && value > 0 ? value : fallback
}

function parseCssPixel(value: string) {
  const parsed = Number.parseFloat(value)
  return Number.isFinite(parsed) ? parsed : 0
}

onMounted(async () => {
  await nextTick()
  safeArea.update()
  restorePosition()
  positionReady.value = true
})

watch(
  bounds,
  () => {
    if (!positionReady.value) return
    restorePosition()
  },
  { flush: 'post' },
)

onScopeDispose(() => {
  window.clearTimeout(ignoreClickTimer)
})
</script>

<template>
  <div
    ref="toggleEl"
    class="fixed top-0 left-0 z-40 min-[961px]:hidden"
    :class="positionReady ? 'opacity-100' : 'opacity-0'"
    :style="toggleStyle"
  >
    <button
      type="button"
      class="inline-flex size-11.5 items-center justify-center rounded-(--cp-icon-button-radius) border-0 bg-(--cp-bg-surface) text-(--cp-text-primary) shadow-[0_18px_34px_-18px_var(--cp-shadow-sticky)] outline-none transition-[background-color,box-shadow,color,opacity,transform] duration-150 hover:bg-(--cp-default-bg-hover) hover:text-(--cp-normal) active:scale-95 focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-page)"
      :class="
        isDragging
          ? 'scale-95 cursor-grabbing shadow-[0_14px_28px_-20px_var(--cp-shadow-sticky)]'
          : 'cursor-grab'
      "
      aria-label="打开侧边栏"
      @pointerdown="handlePointerDown"
      @pointerup="handlePointerUp"
      @click="handleClick"
    >
      <Menu class="size-5" />
    </button>
  </div>
</template>
