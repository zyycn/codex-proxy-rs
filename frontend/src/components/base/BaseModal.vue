<script setup lang="ts">
import { AlertCircle, AlertTriangle, CheckCircle2, Info, X } from '@lucide/vue'
import { computed, nextTick, onBeforeUnmount, useTemplateRef, watch } from 'vue'

import { lockBodyScroll, unlockBodyScroll } from '@/utils/body-scroll-lock'
import BaseButton from './BaseButton.vue'
import BaseScrollbar from './BaseScrollbar.vue'

type ModalVariant = 'default' | 'info' | 'warning' | 'danger' | 'success'

const props = defineProps<{
  title: string
  description?: string
  width?: string | number
  variant?: ModalVariant
  closeDisabled?: boolean
  bodyMaxHeight?: string
  bodyViewClass?: string
  hideFooter?: boolean
}>()

const open = defineModel<boolean>({ default: false })
const panel = useTemplateRef<HTMLElement>('panel')
let previouslyFocused: HTMLElement | null = null
let ownsScrollLock = false

const focusableSelector = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',')

const variant = computed(() => props.variant ?? 'default')

const modalStyle = computed(() => {
  const preferredWidth
    = typeof props.width === 'number' ? `${props.width}px` : (props.width ?? '560px')

  return {
    width: `min(${preferredWidth}, calc(100dvw - 1.5rem))`,
  }
})
const bodyClass = computed(() => [
  'min-h-0 overflow-hidden p-4 sm:p-7',
  props.description || variant.value !== 'default' ? 'pt-4 sm:pt-5' : 'pt-4 sm:pt-6',
])
const scrollViewClass = computed(() =>
  ['pr-3 sm:pr-4', props.bodyViewClass].filter(Boolean).join(' '),
)

const iconMap = {
  default: Info,
  info: Info,
  warning: AlertTriangle,
  danger: AlertCircle,
  success: CheckCircle2,
}

const variantClasses: Record<ModalVariant, { iconBg: string, icon: string }> = {
  default: {
    iconBg: 'bg-(--cp-info-bg)',
    icon: 'text-(--cp-info)',
  },
  info: {
    iconBg: 'bg-(--cp-info-bg)',
    icon: 'text-(--cp-info)',
  },
  warning: {
    iconBg: 'bg-(--cp-warning-bg)',
    icon: 'text-(--cp-warning)',
  },
  danger: {
    iconBg: 'bg-(--cp-danger-bg)',
    icon: 'text-(--cp-danger)',
  },
  success: {
    iconBg: 'bg-(--cp-success-bg)',
    icon: 'text-(--cp-success)',
  },
}

function closeModal() {
  if (props.closeDisabled)
    return
  open.value = false
}

function focusableElements() {
  return Array.from(panel.value?.querySelectorAll<HTMLElement>(focusableSelector) ?? []).filter(
    element => !element.hidden && element.getAttribute('aria-hidden') !== 'true',
  )
}

function handleKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    event.preventDefault()
    closeModal()
    return
  }
  if (event.key !== 'Tab')
    return

  const focusable = focusableElements()
  if (focusable.length === 0) {
    event.preventDefault()
    panel.value?.focus()
    return
  }
  const first = focusable[0]
  const last = focusable[focusable.length - 1]
  if (!panel.value?.contains(document.activeElement)) {
    event.preventDefault()
    if (event.shiftKey)
      last?.focus()
    else first?.focus()
  }
  else if (event.shiftKey && document.activeElement === first) {
    event.preventDefault()
    last?.focus()
  }
  else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault()
    first?.focus()
  }
}

function acquireScrollLock() {
  if (ownsScrollLock)
    return
  ownsScrollLock = true
  lockBodyScroll()
}

function releaseScrollLock() {
  if (!ownsScrollLock)
    return
  ownsScrollLock = false
  unlockBodyScroll()
}

function restorePreviousFocus() {
  if (previouslyFocused?.isConnected)
    previouslyFocused.focus()
  previouslyFocused = null
}

watch(
  open,
  async (isOpen) => {
    if (isOpen) {
      previouslyFocused
        = document.activeElement instanceof HTMLElement ? document.activeElement : null
      acquireScrollLock()
      await nextTick()
      const first = focusableElements()[0]
      if (first)
        first.focus()
      else panel.value?.focus()
      return
    }

    releaseScrollLock()
    restorePreviousFocus()
  },
  { immediate: true },
)

onBeforeUnmount(() => {
  releaseScrollLock()
  restorePreviousFocus()
})
</script>

<template>
  <Teleport to="body">
    <Transition name="cp-modal">
      <div
        v-if="open"
        class="fixed inset-0 z-50 grid place-items-center overflow-hidden p-3 sm:p-6"
        role="presentation"
        @keydown="handleKeydown"
      >
        <button
          type="button"
          tabindex="-1"
          aria-label="关闭弹窗"
          class="absolute inset-0 cursor-default border-0 bg-(--cp-overlay-scrim) p-0"
          @click="closeModal"
        />
        <section
          ref="panel"
          class="cp-modal-panel [--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] relative grid max-h-[calc(100dvh-1.5rem)] min-w-0 grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden rounded-(--cp-card-radius) bg-(--cp-bg-surface) shadow-(--cp-shadow-popover) sm:max-h-[calc(100dvh-3rem)]"
          :style="modalStyle"
          role="dialog"
          aria-modal="true"
          :aria-label="title"
          tabindex="-1"
        >
          <header
            class="grid shrink-0 grid-cols-[auto_minmax(0,1fr)_28px] gap-3 p-4 pb-0 sm:gap-4 sm:p-7 sm:pb-0"
          >
            <span
              v-if="variant !== 'default'"
              class="inline-flex size-11 items-center justify-center rounded-(--cp-icon-button-radius)"
              :class="$slots.icon ? 'bg-(--cp-bg-subtle)' : variantClasses[variant].iconBg"
            >
              <slot name="icon">
                <component :is="iconMap[variant]" :size="18" :class="variantClasses[variant].icon" />
              </slot>
            </span>
            <div class="min-w-0" :class="variant === 'default' ? 'col-span-2' : ''">
              <h2 class="m-0 text-lg leading-[1.15] font-[760] text-(--cp-text-primary)">
                {{ title }}
              </h2>
              <p
                v-if="description"
                class="mt-2 mb-0 text-[13px] leading-[1.45] font-semibold text-(--cp-text-secondary)"
              >
                {{ description }}
              </p>
            </div>
            <BaseButton
              icon-only
              class="col-start-3"
              label="关闭"
              size="sm"
              variant="ghost"
              :disabled="closeDisabled"
              @click="closeModal"
            >
              <X :size="16" />
            </BaseButton>
          </header>
          <div :class="bodyClass">
            <BaseScrollbar
              class="h-full -mr-3 sm:-mr-4"
              :max-height="bodyMaxHeight"
              :view-class="scrollViewClass"
            >
              <slot />
            </BaseScrollbar>
          </div>
          <footer
            v-if="!hideFooter"
            class="flex shrink-0 flex-wrap justify-end gap-2 px-4 pb-4 sm:gap-3 sm:px-7 sm:pb-7"
          >
            <slot name="footer">
              <BaseButton variant="default" @click="open = false">
                取消
              </BaseButton>
              <BaseButton variant="primary" @click="open = false">
                确认
              </BaseButton>
            </slot>
          </footer>
        </section>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.cp-modal-enter-active,
.cp-modal-leave-active {
  transition: opacity 180ms ease;
}

.cp-modal-enter-active .cp-modal-panel,
.cp-modal-leave-active .cp-modal-panel {
  transition:
    opacity 180ms ease,
    transform 180ms ease;
}

.cp-modal-enter-from,
.cp-modal-leave-to {
  opacity: 0;
}

.cp-modal-enter-from .cp-modal-panel {
  opacity: 0;
  transform: translate3d(0, 8px, 0) scale(0.985);
}

.cp-modal-leave-to .cp-modal-panel {
  opacity: 0;
  transform: translate3d(0, 4px, 0) scale(0.99);
}

@media (prefers-reduced-motion: reduce) {
  .cp-modal-enter-active,
  .cp-modal-leave-active,
  .cp-modal-enter-active .cp-modal-panel,
  .cp-modal-leave-active .cp-modal-panel {
    transition: none;
  }
}
</style>
