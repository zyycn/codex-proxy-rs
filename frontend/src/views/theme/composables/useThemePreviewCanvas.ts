import type { ShallowRef } from 'vue'
import { useResizeObserver } from '@vueuse/core'
import { gsap } from 'gsap'
import { Draggable } from 'gsap/Draggable'
import { computed, nextTick, onBeforeUnmount, onMounted, readonly, shallowRef } from 'vue'

gsap.registerPlugin(Draggable)

interface ThemePreviewCanvasOptions {
  viewport: ShallowRef<HTMLElement | null>
  stage: ShallowRef<HTMLElement | null>
  boardWidth: number
  boardHeight: number
}

const minimumScale = 0.25
const maximumScale = 1.5
const zoomStep = 0.1
const scaleGrid = 0.05
const viewportPadding = 48

export function useThemePreviewCanvas(options: ThemePreviewCanvasOptions) {
  const scale = shallowRef(1)
  const x = shallowRef(0)
  const y = shallowRef(0)
  const ready = shallowRef(false)
  const fitted = shallowRef(true)
  const dragging = shallowRef(false)
  let draggable: Draggable | undefined

  const scalePercent = computed(() => Math.round(scale.value * 100))
  const canZoomOut = computed(() => scale.value > minimumScale)
  const canZoomIn = computed(() => scale.value < maximumScale)

  function clampScale(value: number) {
    const clamped = Math.min(maximumScale, Math.max(minimumScale, value))
    return Math.round(Math.round(clamped / scaleGrid) * scaleGrid * 100) / 100
  }

  function applyPosition() {
    const stage = options.stage.value
    if (!stage)
      return

    gsap.killTweensOf(stage)
    gsap.set(stage, {
      left: x.value,
      top: y.value,
      clearProps: 'transform',
    })
    draggable?.update()
  }

  function viewportCenter() {
    const rect = options.viewport.value?.getBoundingClientRect()
    return {
      x: (rect?.width ?? 0) / 2,
      y: (rect?.height ?? 0) / 2,
    }
  }

  function setScale(nextScale: number, anchor = viewportCenter()) {
    const normalizedScale = clampScale(nextScale)
    if (normalizedScale === scale.value)
      return

    const previousScale = scale.value
    const boardX = (anchor.x - x.value) / previousScale
    const boardY = (anchor.y - y.value) / previousScale

    scale.value = normalizedScale
    x.value = Math.round(anchor.x - boardX * normalizedScale)
    y.value = Math.round(anchor.y - boardY * normalizedScale)
    fitted.value = false
    applyPosition()
  }

  function zoomIn() {
    setScale(scale.value + zoomStep)
  }

  function zoomOut() {
    setScale(scale.value - zoomStep)
  }

  function resetScale() {
    setScale(1)
  }

  function fitToViewport() {
    const viewport = options.viewport.value
    if (!viewport)
      return

    const rect = viewport.getBoundingClientRect()
    const availableWidth = Math.max(0, rect.width - viewportPadding * 2)
    const availableHeight = Math.max(0, rect.height - viewportPadding * 2)
    const rawScale = Math.min(
      availableWidth / options.boardWidth,
      availableHeight / options.boardHeight,
    )
    const nextScale = clampScale(Math.floor(rawScale / scaleGrid) * scaleGrid)

    scale.value = nextScale
    x.value = Math.round((rect.width - options.boardWidth * nextScale) / 2)
    y.value = Math.round((rect.height - options.boardHeight * nextScale) / 2)
    fitted.value = true
    applyPosition()
  }

  function handleWheel(event: WheelEvent) {
    event.preventDefault()
    const viewport = options.viewport.value
    if (!viewport)
      return

    const rect = viewport.getBoundingClientRect()
    const direction = event.deltaY > 0 ? -1 : 1
    setScale(
      scale.value + direction * zoomStep,
      { x: event.clientX - rect.left, y: event.clientY - rect.top },
    )
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === '+' || event.key === '=') {
      event.preventDefault()
      zoomIn()
      return
    }
    if (event.key === '-') {
      event.preventDefault()
      zoomOut()
      return
    }
    if (event.key === '0') {
      event.preventDefault()
      fitToViewport()
    }
  }

  function handleDoubleClick() {
    fitToViewport()
  }

  function createDraggable() {
    const viewport = options.viewport.value
    const stage = options.stage.value
    if (!viewport || !stage)
      return

    draggable = Draggable.create(stage, {
      type: 'left,top',
      trigger: viewport,
      cursor: 'grab',
      activeCursor: 'grabbing',
      dragClickables: false,
      zIndexBoost: false,
      liveSnap: {
        x: value => Math.round(value),
        y: value => Math.round(value),
      },
      onPress() {
        dragging.value = true
        fitted.value = false
        gsap.killTweensOf(stage)
      },
      onDrag() {
        x.value = Math.round(this.x)
        y.value = Math.round(this.y)
      },
      onRelease() {
        dragging.value = false
      },
      onDragEnd() {
        dragging.value = false
      },
    })[0]
  }

  useResizeObserver(options.viewport, () => {
    if (ready.value && fitted.value)
      fitToViewport()
  })

  onMounted(async () => {
    await nextTick()
    options.viewport.value?.addEventListener('wheel', handleWheel, { passive: false })
    options.viewport.value?.addEventListener('keydown', handleKeydown)
    options.viewport.value?.addEventListener('dblclick', handleDoubleClick)
    createDraggable()
    fitToViewport()
    ready.value = true
  })

  onBeforeUnmount(() => {
    draggable?.kill()
    options.viewport.value?.removeEventListener('wheel', handleWheel)
    options.viewport.value?.removeEventListener('keydown', handleKeydown)
    options.viewport.value?.removeEventListener('dblclick', handleDoubleClick)
    if (options.stage.value)
      gsap.killTweensOf(options.stage.value)
  })

  return {
    canZoomIn,
    canZoomOut,
    dragging,
    fitToViewport,
    resetScale,
    scale: readonly(scale),
    scalePercent,
    zoomIn,
    zoomOut,
  }
}
