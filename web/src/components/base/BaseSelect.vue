<script setup lang="ts">
import { Check, ChevronDown } from '@lucide/vue'
import { computed, nextTick, onBeforeUnmount, onMounted, ref, useAttrs, watch } from 'vue'
import type { CSSProperties } from 'vue'

interface SelectOption {
  label: string
  value: string
  disabled?: boolean
}

type SelectSize = 'default' | 'pagination'

defineOptions({
  inheritAttrs: false,
})

const props = withDefaults(
  defineProps<{
    options: SelectOption[]
    size?: SelectSize
    disabled?: boolean
    placeholder?: string
    emptyText?: string
  }>(),
  {
    size: 'default',
    disabled: false,
    placeholder: '请选择',
    emptyText: '暂无选项',
  },
)

const model = defineModel<string>({ required: true })
const attrs = useAttrs()

const rootRef = ref<HTMLElement | null>(null)
const triggerRef = ref<HTMLButtonElement | null>(null)
const popoverRef = ref<HTMLElement | null>(null)
const open = ref(false)
const activeIndex = ref(-1)
const popoverStyle = ref<CSSProperties>({})
const selectId = `base-select-${Math.random().toString(36).slice(2)}`

const sizeConfig: Record<
  SelectSize,
  {
    trigger: string
    option: string
    icon: number
  }
> = {
  default: {
    trigger:
      'h-(--cp-input-height-default) px-3.5 pr-9 text-[13px] rounded-(--cp-input-radius-base)',
    option: 'h-8.5 px-3 text-[13px]',
    icon: 16,
  },
  pagination: {
    trigger: 'h-8 px-2.5 pr-7 text-xs rounded-(--cp-input-radius-base)',
    option: 'h-8 px-2.5 text-xs',
    icon: 14,
  },
}

const selectedOption = computed(() => props.options.find((option) => option.value === model.value))

const triggerClasses = computed(() => [
  'relative inline-flex w-full min-w-0 items-center gap-2 overflow-visible border-0 text-left font-[650] leading-none shadow-(--cp-shadow-input) outline-none transition-[background-color,box-shadow,color] duration-[160ms]',
  sizeConfig[props.size].trigger,
  props.disabled
    ? 'cursor-not-allowed bg-(--cp-disabled-bg) text-(--cp-disabled-text) shadow-none'
    : open.value
      ? 'cursor-pointer bg-(--cp-input-soft-bg-focus) text-(--cp-text-primary) shadow-(--cp-shadow-input-focus)'
      : [
          'cursor-pointer bg-[var(--cp-input-current-bg,var(--cp-input-context-bg))] text-(--cp-text-primary)',
          'hover:bg-[var(--cp-input-current-bg-hover,var(--cp-input-context-bg-hover))] hover:shadow-(--cp-shadow-input-hover)',
          'focus-visible:bg-(--cp-input-soft-bg-focus) focus-visible:shadow-(--cp-shadow-input-focus)',
        ],
])

const popoverClasses = computed(() => [
  'fixed z-50 flex flex-col gap-1 rounded-(--cp-popover-radius) border-0 bg-(--cp-bg-surface) p-1 shadow-(--cp-shadow-popover)',
  props.options.length > 6 ? 'cp-scrollbar overflow-y-auto' : 'overflow-visible',
])

function optionId(index: number) {
  return `${selectId}-option-${index}`
}

function enabledIndexes() {
  return props.options.flatMap((option, index) => (option.disabled ? [] : [index]))
}

function selectedIndex() {
  return props.options.findIndex((option) => option.value === model.value)
}

function setActiveToSelected() {
  const selected = selectedIndex()
  if (selected >= 0 && !props.options[selected]?.disabled) {
    activeIndex.value = selected
    return
  }

  activeIndex.value = enabledIndexes()[0] ?? -1
}

function updatePopoverPosition() {
  if (!open.value || !triggerRef.value) return

  const rect = triggerRef.value.getBoundingClientRect()
  const gap = 6
  const estimatedMenuHeight = Math.min(Math.max(props.options.length, 1) * 34 + 8, 244)
  const belowSpace = window.innerHeight - rect.bottom - gap
  const aboveSpace = rect.top - gap
  const placeAbove = belowSpace < estimatedMenuHeight && aboveSpace > belowSpace
  const availableHeight = Math.max(placeAbove ? aboveSpace : belowSpace, 120)
  const maxHeight = Math.min(estimatedMenuHeight, availableHeight)
  const top = placeAbove
    ? Math.max(8, rect.top - maxHeight - gap)
    : Math.min(rect.bottom + gap, window.innerHeight - maxHeight - 8)
  const left = Math.max(8, Math.min(rect.left, window.innerWidth - rect.width - 8))

  popoverStyle.value = {
    left: `${left}px`,
    top: `${top}px`,
    width: `${rect.width}px`,
    maxHeight: `${maxHeight}px`,
  }
}

async function openMenu() {
  if (props.disabled || open.value) return

  open.value = true
  setActiveToSelected()
  await nextTick()
  updatePopoverPosition()
}

function closeMenu() {
  open.value = false
}

function toggleMenu() {
  if (open.value) {
    closeMenu()
    return
  }

  void openMenu()
}

function moveActive(delta: number) {
  const indexes = enabledIndexes()
  if (indexes.length === 0) return

  const current = indexes.indexOf(activeIndex.value)
  const next = current === -1 ? (delta > 0 ? 0 : indexes.length - 1) : current + delta
  activeIndex.value = indexes[(next + indexes.length) % indexes.length]
}

function chooseOption(option: SelectOption, index: number) {
  if (option.disabled) return

  model.value = option.value
  activeIndex.value = index
  closeMenu()
}

function chooseActive() {
  const option = props.options[activeIndex.value]
  if (!option) return

  chooseOption(option, activeIndex.value)
}

function handleTriggerKeydown(event: KeyboardEvent) {
  if (props.disabled) return

  if (event.key === 'ArrowDown') {
    event.preventDefault()
    if (!open.value) {
      void openMenu()
      return
    }
    moveActive(1)
    return
  }

  if (event.key === 'ArrowUp') {
    event.preventDefault()
    if (!open.value) {
      void openMenu()
      return
    }
    moveActive(-1)
    return
  }

  if (event.key === 'Enter' || event.key === ' ') {
    event.preventDefault()
    if (!open.value) {
      void openMenu()
      return
    }
    chooseActive()
    return
  }

  if (event.key === 'Escape') {
    closeMenu()
  }
}

function handlePointerDown(event: PointerEvent) {
  const target = event.target as Node | null
  if (!target) return

  if (rootRef.value?.contains(target) || popoverRef.value?.contains(target)) return
  closeMenu()
}

function optionClasses(option: SelectOption, index: number) {
  return [
    'flex w-full items-center gap-2 rounded-(--cp-input-radius-small) border-0 px-3 text-left font-[650] leading-none outline-none transition-colors',
    sizeConfig[props.size].option,
    option.disabled
      ? 'cursor-not-allowed bg-transparent text-(--cp-disabled-text)'
      : option.value === model.value
        ? 'cursor-pointer bg-(--cp-info-bg) text-(--cp-info-text)'
        : activeIndex.value === index
          ? 'cursor-pointer bg-(--cp-default-bg-hover) text-(--cp-text-primary)'
          : 'cursor-pointer bg-transparent text-(--cp-text-primary) hover:bg-(--cp-default-bg-hover)',
  ]
}

watch(open, async (isOpen) => {
  if (!isOpen) return

  await nextTick()
  updatePopoverPosition()
})

watch(
  () => [props.options, model.value],
  () => {
    if (!open.value) return
    setActiveToSelected()
  },
)

onMounted(() => {
  document.addEventListener('pointerdown', handlePointerDown)
  window.addEventListener('resize', updatePopoverPosition)
  window.addEventListener('scroll', updatePopoverPosition, true)
})

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', handlePointerDown)
  window.removeEventListener('resize', updatePopoverPosition)
  window.removeEventListener('scroll', updatePopoverPosition, true)
})
</script>

<template>
  <div
    ref="rootRef"
    class="relative inline-block box-content overflow-visible p-0.75 text-left"
    v-bind="attrs"
  >
    <button
      :id="selectId"
      ref="triggerRef"
      type="button"
      :class="triggerClasses"
      :disabled="disabled"
      role="combobox"
      :aria-expanded="open"
      :aria-controls="`${selectId}-listbox`"
      :aria-activedescendant="open && activeIndex >= 0 ? optionId(activeIndex) : undefined"
      @click="toggleMenu"
      @keydown="handleTriggerKeydown"
    >
      <span class="min-w-0 flex-1 truncate">
        {{ selectedOption?.label ?? placeholder }}
      </span>
      <ChevronDown
        class="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 transition-transform"
        :class="
          disabled
            ? 'text-(--cp-disabled-icon)'
            : open
              ? 'rotate-180 text-(--cp-info)'
              : 'text-(--cp-text-muted)'
        "
        :size="sizeConfig[size].icon"
      />
    </button>

    <Teleport to="body">
      <Transition
        enter-active-class="transition-[opacity,transform] duration-150 ease-out"
        enter-from-class="-translate-y-1 opacity-0"
        enter-to-class="translate-y-0 opacity-100"
        leave-active-class="transition-opacity duration-150 ease-in"
        leave-from-class="opacity-100"
        leave-to-class="opacity-0"
      >
        <div
          v-if="open"
          :id="`${selectId}-listbox`"
          ref="popoverRef"
          :class="popoverClasses"
          :style="popoverStyle"
          role="listbox"
          :aria-labelledby="selectId"
        >
          <div
            v-if="options.length === 0"
            class="flex h-8.5 items-center rounded-(--cp-input-radius-small) px-3 text-[13px] leading-none font-[650] text-(--cp-text-muted)"
          >
            {{ emptyText }}
          </div>

          <template v-else>
            <button
              v-for="(option, index) in options"
              :id="optionId(index)"
              :key="option.value"
              type="button"
              role="option"
              :aria-selected="option.value === model"
              :disabled="option.disabled"
              :class="optionClasses(option, index)"
              @mouseenter="activeIndex = option.disabled ? activeIndex : index"
              @mousedown.prevent
              @click="chooseOption(option, index)"
            >
              <span class="min-w-0 flex-1 truncate">{{ option.label }}</span>
              <Check
                v-if="option.value === model"
                class="shrink-0 text-(--cp-info)"
                :size="size === 'pagination' ? 13 : 15"
              />
            </button>
          </template>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>
