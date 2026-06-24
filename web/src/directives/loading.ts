import type { Directive, DirectiveBinding } from 'vue'

type LoadingBindingValue =
  | boolean
  | {
      loading?: boolean
      text?: string
    }

interface LoadingState {
  overlay: HTMLDivElement
  previousPosition: string
  managedPosition: boolean
}

type LoadingElement = HTMLElement & {
  __cpLoading?: LoadingState
}

const OVERLAY_CLASS = [
  'absolute',
  'inset-0',
  'z-20',
  'grid',
  'min-h-18',
  'place-items-center',
  'rounded-[inherit]',
  'bg-(--cp-bg-surface)/82',
  'backdrop-blur-[4px]',
].join(' ')
const PANEL_CLASS = [
  'inline-flex',
  'min-h-10',
  'items-center',
  'gap-2.5',
  'rounded-xl',
  'bg-(--cp-bg-surface)',
  'py-2.25',
  'pr-3.5',
  'pl-3',
  'text-xs',
  'leading-[1.15]',
  'font-[760]',
  'text-(--cp-text-secondary)',
  'shadow-(--cp-shadow-popover)',
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
    }
  }

  return {
    loading: Boolean(value),
    text: '加载中',
  }
}

function createOverlay(text: string) {
  const overlay = document.createElement('div')
  overlay.className = OVERLAY_CLASS
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

function showLoading(element: LoadingElement, text: string) {
  if (element.__cpLoading) {
    const label = element.__cpLoading.overlay.querySelector<HTMLElement>('[data-loading-label]')
    if (label) label.textContent = text
    return
  }

  const previousPosition = element.style.position
  const managedPosition = window.getComputedStyle(element).position === 'static'
  if (managedPosition) {
    element.style.position = 'relative'
  }

  const overlay = createOverlay(text)
  element.append(overlay)
  element.setAttribute('aria-busy', 'true')
  element.classList.add('overflow-hidden')
  element.__cpLoading = {
    overlay,
    previousPosition,
    managedPosition,
  }
}

function hideLoading(element: LoadingElement) {
  const state = element.__cpLoading
  if (!state) return

  state.overlay.remove()
  element.removeAttribute('aria-busy')
  element.classList.remove('overflow-hidden')
  if (state.managedPosition) {
    element.style.position = state.previousPosition
  }
  element.__cpLoading = undefined
}

function syncLoading(element: LoadingElement, binding: DirectiveBinding<LoadingBindingValue>) {
  const { loading, text } = normalizeBinding(binding.value)
  if (loading) {
    showLoading(element, text)
  } else {
    hideLoading(element)
  }
}

export const loadingDirective: Directive<LoadingElement, LoadingBindingValue> = {
  mounted: syncLoading,
  updated: syncLoading,
  beforeUnmount: hideLoading,
}
