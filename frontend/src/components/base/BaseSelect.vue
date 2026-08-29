<script setup lang="ts">
import type { CSSProperties } from 'vue'

import { Check, ChevronDown } from '@lucide/vue'
import { onClickOutside, useEventListener, useThrottleFn, whenever } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { computed, inject, nextTick, ref, useAttrs, useId, watch } from 'vue'
import { formFieldKey } from './BaseForm/context'

type SelectSize = 'sm' | 'md' | 'lg'

export interface SelectOption {
  label: string
  value: string
  disabled?: boolean
}

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
    size: 'md',
    disabled: false,
    placeholder: '请选择',
    emptyText: '暂无选项',
  },
)

const model = defineModel<string>({ required: true })
const attrs = useAttrs()
const field = inject(formFieldKey, null)

const rootRef = ref<HTMLElement | null>(null)
const triggerRef = ref<HTMLButtonElement | null>(null)
const popoverRef = ref<HTMLElement | null>(null)
const open = ref(false)
const activeIndex = ref(-1)
const popoverStyle = ref<CSSProperties>({})
const selectId = `base-select-${useId()}`
const controlId = computed(() => (typeof attrs.id === 'string' ? attrs.id : (field?.controlId.value ?? selectId)))
const invalid = computed(() =>
  Boolean(field?.invalid.value || attrs['aria-invalid'] === true || attrs['aria-invalid'] === 'true'),
)
const describedBy = computed(
  () =>
    [typeof attrs['aria-describedby'] === 'string' ? attrs['aria-describedby'] : undefined, field?.describedBy.value]
      .filter(Boolean)
      .join(' ') || undefined,
)
const rootAttrs = computed(() => ({ class: attrs.class, style: attrs.style }))
const triggerAttrs = computed(() =>
  Object.fromEntries(
    Object.entries(attrs).filter(
      ([key]) => !['class', 'style', 'id', 'aria-describedby', 'aria-invalid', 'aria-required'].includes(key),
    ),
  ),
)

const sizeConfig: Record<
  SelectSize,
  {
    trigger: string
    option: string
    icon: number
  }
> = {
  md: {
    trigger: 'h-cp-control px-3.5 pr-9 text-cp rounded-cp',
    option: 'h-8.5 px-3 text-cp',
    icon: 16,
  },
  sm: {
    trigger: 'h-cp-control-sm px-2.5 pr-7 text-xs rounded-cp',
    option: 'h-8 px-2.5 text-xs',
    icon: 14,
  },
  lg: {
    trigger: 'h-cp-control-lg px-4 pr-10 text-cp-lg rounded-cp',
    option: 'h-10 px-3.5 text-cp-lg',
    icon: 17,
  },
}

const selectedOption = computed(() => props.options.find(option => option.value === model.value))

const triggerClasses = computed(() => [
  'relative inline-flex w-full min-w-0 items-center gap-2 overflow-visible border-0 text-left font-emphasis leading-none shadow-cp-input outline-none transition-[background-color,box-shadow,color] duration-[160ms]',
  sizeConfig[props.size].trigger,
  props.disabled
    ? 'cursor-not-allowed bg-cp-bg-container-disabled text-cp-text-disabled shadow-none'
    : invalid.value
      ? 'cursor-pointer bg-(--cp-input-error-active-bg) text-cp-error-text shadow-cp-input-error-active'
      : open.value
        ? 'cursor-pointer bg-(--cp-input-active-bg) text-cp-text shadow-cp-input-active'
        : [
            'cursor-pointer bg-[var(--cp-input-bg)] text-cp-text',
            'hover:bg-[var(--cp-input-hover-bg)] hover:shadow-cp-input-hover',
            'focus-visible:bg-(--cp-input-active-bg) focus-visible:shadow-cp-input-active',
          ],
])

const popoverClasses = computed(() => [
  'fixed z-50 flex flex-col gap-1 rounded-cp-lg border-0 bg-cp-bg-elevated p-1 shadow-cp',
  props.options.length > 6 ? 'cp-scrollbar overflow-y-auto' : 'overflow-visible',
])

function optionId(index: number) {
  return `${selectId}-option-${index}`
}

function enabledIndexes() {
  return props.options.flatMap((option, index) => (option.disabled ? [] : [index]))
}

function selectedIndex() {
  return props.options.findIndex(option => option.value === model.value)
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
  if (!open.value || !triggerRef.value)
    return

  const rect = triggerRef.value.getBoundingClientRect()
  const gap = 6
  const estimatedMenuHeight = clamp(props.options.length * 34 + 8, 42, 244)
  const belowSpace = window.innerHeight - rect.bottom - gap
  const aboveSpace = rect.top - gap
  const placeAbove = belowSpace < estimatedMenuHeight && aboveSpace > belowSpace
  const availableHeight = Math.max(placeAbove ? aboveSpace : belowSpace, 120)
  const maxHeight = clamp(estimatedMenuHeight, 0, availableHeight)
  const top = placeAbove
    ? Math.max(8, rect.top - maxHeight - gap)
    : Math.min(rect.bottom + gap, window.innerHeight - maxHeight - 8)
  const left = clamp(rect.left, 8, window.innerWidth - rect.width - 8)

  popoverStyle.value = {
    left: `${left}px`,
    top: `${top}px`,
    width: `${rect.width}px`,
    maxHeight: `${maxHeight}px`,
  }
}

const updatePopoverPositionThrottled = useThrottleFn(updatePopoverPosition, 32, true)

async function openMenu() {
  if (props.disabled || open.value)
    return

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
  if (indexes.length === 0)
    return

  const current = indexes.indexOf(activeIndex.value)
  const next = current === -1 ? (delta > 0 ? 0 : indexes.length - 1) : current + delta
  activeIndex.value = indexes[(next + indexes.length) % indexes.length]
}

function chooseOption(option: SelectOption, index: number) {
  if (option.disabled)
    return

  model.value = option.value
  activeIndex.value = index
  closeMenu()
}

function chooseActive() {
  const option = props.options[activeIndex.value]
  if (!option)
    return

  chooseOption(option, activeIndex.value)
}

function handleTriggerKeydown(event: KeyboardEvent) {
  if (props.disabled)
    return

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

function optionClasses(option: SelectOption, index: number) {
  return [
    'flex w-full touch-manipulation items-center gap-2 rounded-cp-sm border-0 px-3 text-left font-emphasis leading-none outline-none transition-colors motion-reduce:transition-none',
    sizeConfig[props.size].option,
    option.disabled
      ? 'cursor-not-allowed bg-transparent text-cp-text-disabled'
      : option.value === model.value
        ? 'cursor-pointer bg-cp-control-item-bg-active text-cp-primary-text'
        : activeIndex.value === index
          ? 'cursor-pointer bg-cp-bg-text-hover text-cp-text'
          : 'cursor-pointer bg-transparent text-cp-text hover:bg-cp-bg-text-hover',
  ]
}

whenever(open, async () => {
  await nextTick()
  updatePopoverPosition()
})

watch(
  () => [props.options, model.value],
  () => {
    if (!open.value)
      return
    setActiveToSelected()
  },
)

onClickOutside(rootRef, closeMenu, { ignore: [popoverRef] })
useEventListener(window, 'resize', updatePopoverPositionThrottled)
useEventListener(window, 'scroll', updatePopoverPositionThrottled, { capture: true })
</script>

<template>
  <div ref="rootRef" class="relative inline-block text-left" v-bind="rootAttrs">
    <button
      v-bind="triggerAttrs"
      :id="controlId"
      ref="triggerRef"
      type="button"
      :class="triggerClasses"
      :disabled="disabled"
      role="combobox"
      :aria-expanded="open"
      :aria-controls="`${selectId}-listbox`"
      :aria-activedescendant="open && activeIndex >= 0 ? optionId(activeIndex) : undefined"
      :aria-describedby="describedBy"
      :aria-invalid="invalid || undefined"
      :aria-required="field?.required.value || undefined"
      @click="toggleMenu"
      @keydown="handleTriggerKeydown"
    >
      <span class="min-w-0 flex-1 truncate">
        {{ selectedOption?.label ?? placeholder }}
      </span>
      <ChevronDown
        class="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 transition-transform"
        :class="
          disabled ? 'text-cp-text-disabled' : open ? 'rotate-180 text-cp-primary-text' : 'text-cp-text-quaternary'
        "
        :size="sizeConfig[size].icon"
      />
    </button>

    <Teleport to="body">
      <Transition
        enter-active-class="transition-[opacity,transform] duration-150 ease-out motion-reduce:transition-none"
        enter-from-class="-translate-y-1 opacity-0"
        enter-to-class="translate-y-0 opacity-100"
        leave-active-class="transition-opacity duration-150 ease-in motion-reduce:transition-none"
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
          :aria-labelledby="controlId"
        >
          <div
            v-if="options.length === 0"
            class="flex h-8.5 items-center rounded-cp-sm px-3 text-cp leading-none font-emphasis text-cp-text-quaternary"
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
              @focus="activeIndex = option.disabled ? activeIndex : index"
              @mousedown.prevent
              @click="chooseOption(option, index)"
            >
              <span class="min-w-0 flex-1 truncate">{{ option.label }}</span>
              <Check
                v-if="option.value === model"
                class="shrink-0 text-cp-primary-text"
                :size="size === 'sm' ? 13 : size === 'lg' ? 17 : 15"
              />
            </button>
          </template>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>
