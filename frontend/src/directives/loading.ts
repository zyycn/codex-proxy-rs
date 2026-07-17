import type { Directive, DirectiveBinding } from 'vue'

type LoadingBindingValue
  = | boolean
    | {
      loading?: boolean
      text?: string
      preserveContent?: boolean
    }

interface LoadingState {
  overlay: HTMLDivElement
  previousPosition: string
  managedPosition: boolean
  preserveContent: boolean
}

type LoadingElement = HTMLElement & {
  __cpLoading?: LoadingState
}

const OVERLAY_BASE_CLASS = [
  'absolute',
  'inset-0',
  'z-20',
  'grid',
  'min-h-18',
  'place-items-center',
  'rounded-[inherit]',
].join(' ')
const OVERLAY_MASK_CLASS = 'bg-(--cp-bg-surface)/82 backdrop-blur-[4px]'
const PANEL_CLASS = [
  'inline-flex',
  'min-h-10',
  'items-center',
  'gap-2.5',
  'rounded-xl',
  'py-2.25',
  'pr-3.5',
  'pl-3',
  'text-xs',
  'leading-[1.15]',
  'font-[760]',
  'text-(--cp-text-secondary)',
].join(' ')
const MARK_CLASS = [
  'inline-block',
  'size-4',
  'rounded-full',
  'animate-spin',
  'bg-[radial-gradient(circle_at_center,var(--cp-bg-surface)_0_44%,transparent_46%),conic-gradient(from_0deg,var(--cp-normal)_0deg_105deg,var(--cp-info)_105deg_200deg,var(--cp-bg-muted)_200deg_360deg)]',
].join(' ')
const LABEL_CLASS = 'whitespace-nowrap'

function normalizeBinding(value: LoadingBindingValue | undefined) {
  if (typeof value === 'object' && value !== null) {
    return {
      loading: Boolean(value.loading),
      text: value.text || '加载中',
      preserveContent: Boolean(value.preserveContent),
    }
  }

  return {
    loading: Boolean(value),
    text: '加载中',
    preserveContent: false,
  }
}

function overlayClass(preserveContent: boolean) {
  return preserveContent ? OVERLAY_BASE_CLASS : `${OVERLAY_BASE_CLASS} ${OVERLAY_MASK_CLASS}`
}

function createOverlay(text: string, preserveContent: boolean) {
  const overlay = document.createElement('div')
  overlay.className = overlayClass(preserveContent)
  overlay.setAttribute('role', 'status')
  overlay.setAttribute('aria-live', 'polite')

  const panel = document.createElement('div')
  panel.className = PANEL_CLASS

  const mark = document.createElement('span')
  mark.className = MARK_CLASS
  mark.setAttribute('aria-hidden', 'true')

  const label = document.createElement('span')
  label.className = LABEL_CLASS
  label.dataset.loadingLabel = ''
  label.textContent = text

  panel.append(mark, label)
  overlay.append(panel)

  return overlay
}

function showLoading(element: LoadingElement, text: string, preserveContent: boolean) {
  if (element.__cpLoading) {
    const label = element.__cpLoading.overlay.querySelector<HTMLElement>('[data-loading-label]')
    if (label)
      label.textContent = text
    if (element.__cpLoading.preserveContent !== preserveContent) {
      element.__cpLoading.overlay.className = overlayClass(preserveContent)
      element.__cpLoading.preserveContent = preserveContent
    }
    return
  }

  const previousPosition = element.style.position
  const managedPosition = window.getComputedStyle(element).position === 'static'
  if (managedPosition) {
    element.style.position = 'relative'
  }

  const overlay = createOverlay(text, preserveContent)
  element.append(overlay)
  element.setAttribute('aria-busy', 'true')
  element.classList.add('overflow-hidden')
  element.__cpLoading = {
    overlay,
    previousPosition,
    managedPosition,
    preserveContent,
  }
}

function hideLoading(element: LoadingElement) {
  const state = element.__cpLoading
  if (!state)
    return

  state.overlay.remove()
  element.removeAttribute('aria-busy')
  element.classList.remove('overflow-hidden')
  if (state.managedPosition) {
    element.style.position = state.previousPosition
  }
  element.__cpLoading = undefined
}

function syncLoading(element: LoadingElement, binding: DirectiveBinding<LoadingBindingValue>) {
  const { loading, text, preserveContent } = normalizeBinding(binding.value)
  if (loading) {
    showLoading(element, text, preserveContent)
  }
  else {
    hideLoading(element)
  }
}

export const loading: Directive<LoadingElement, LoadingBindingValue> = {
  mounted: syncLoading,
  updated: syncLoading,
  beforeUnmount: hideLoading,
}
