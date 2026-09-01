import type { ShallowRef } from 'vue'

import { onBeforeUnmount, onMounted, shallowRef } from 'vue'

const VIEWPORT_GUTTER = 8
const interactiveSelector = 'button, a, input, select, textarea, [role="button"], [data-modal-drag-ignore]'

interface DragSession {
  pointerId: number
  startX: number
  startY: number
  originX: number
  originY: number
  minX: number
  maxX: number
  minY: number
  maxY: number
}

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value))
}

export function useModalDrag(
  target: Readonly<ShallowRef<HTMLElement | null>>,
  enabled: () => boolean,
) {
  const isDragging = shallowRef(false)
  const offset = { x: 0, y: 0 }
  let session: DragSession | null = null

  function applyPosition() {
    if (!target.value)
      return

    target.value.style.transform
      = offset.x === 0 && offset.y === 0
        ? ''
        : `translate3d(${offset.x}px, ${offset.y}px, 0)`
  }

  function removeDragListeners() {
    document.removeEventListener('pointermove', handlePointerMove)
    document.removeEventListener('pointerup', endDrag)
    document.removeEventListener('pointercancel', endDrag)
    window.removeEventListener('blur', handleWindowBlur)
  }

  function endDrag(event?: PointerEvent) {
    if (event && session && event.pointerId !== session.pointerId)
      return

    session = null
    isDragging.value = false
    removeDragListeners()
  }

  function handlePointerMove(event: PointerEvent) {
    if (!session || event.pointerId !== session.pointerId)
      return
    if (!enabled()) {
      endDrag()
      return
    }

    event.preventDefault()
    offset.x = clamp(
      session.originX + event.clientX - session.startX,
      session.minX,
      session.maxX,
    )
    offset.y = clamp(
      session.originY + event.clientY - session.startY,
      session.minY,
      session.maxY,
    )
    applyPosition()
  }

  function handlePointerDown(event: PointerEvent) {
    if (!enabled() || !event.isPrimary || event.button !== 0)
      return

    const eventTarget = event.target
    if (eventTarget instanceof Element && eventTarget.closest(interactiveSelector))
      return

    const element = target.value
    if (!element)
      return

    const rect = element.getBoundingClientRect()
    const viewportWidth = document.documentElement.clientWidth
    const viewportHeight = document.documentElement.clientHeight

    session = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      originX: offset.x,
      originY: offset.y,
      minX: offset.x - rect.left + VIEWPORT_GUTTER,
      maxX: offset.x + viewportWidth - rect.right - VIEWPORT_GUTTER,
      minY: offset.y - rect.top + VIEWPORT_GUTTER,
      maxY: offset.y + viewportHeight - rect.bottom - VIEWPORT_GUTTER,
    }
    isDragging.value = true
    event.preventDefault()

    document.addEventListener('pointermove', handlePointerMove, { passive: false })
    document.addEventListener('pointerup', endDrag)
    document.addEventListener('pointercancel', endDrag)
    window.addEventListener('blur', handleWindowBlur)
  }

  function handleWindowBlur() {
    endDrag()
  }

  function updatePosition() {
    const element = target.value
    if (!element)
      return

    const rect = element.getBoundingClientRect()
    const viewportWidth = document.documentElement.clientWidth
    const viewportHeight = document.documentElement.clientHeight

    if (rect.left < VIEWPORT_GUTTER)
      offset.x += VIEWPORT_GUTTER - rect.left
    else if (rect.right > viewportWidth - VIEWPORT_GUTTER)
      offset.x -= rect.right - viewportWidth + VIEWPORT_GUTTER

    if (rect.top < VIEWPORT_GUTTER)
      offset.y += VIEWPORT_GUTTER - rect.top
    else if (rect.bottom > viewportHeight - VIEWPORT_GUTTER)
      offset.y -= rect.bottom - viewportHeight + VIEWPORT_GUTTER

    applyPosition()
  }

  function resetPosition() {
    endDrag()
    offset.x = 0
    offset.y = 0
    applyPosition()
  }

  function handleResize() {
    endDrag()
    updatePosition()
  }

  onMounted(() => window.addEventListener('resize', handleResize))
  onBeforeUnmount(() => {
    endDrag()
    window.removeEventListener('resize', handleResize)
  })

  return {
    cancelDrag: endDrag,
    constrainPosition: updatePosition,
    handlePointerDown,
    isDragging,
    resetPosition,
  }
}
