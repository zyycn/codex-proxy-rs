<script setup lang="ts">
import { computed, inject, useAttrs } from 'vue'
import { formFieldKey } from './BaseForm/context'

type TextareaSize = 'sm' | 'md' | 'lg'

defineOptions({ inheritAttrs: false })

const props = withDefaults(
  defineProps<{
    placeholder?: string
    disabled?: boolean
    size?: TextareaSize
    rows?: number
  }>(),
  {
    placeholder: '',
    disabled: false,
    size: 'md',
    rows: 5,
  },
)

const model = defineModel<string>({ default: '' })
const attrs = useAttrs()
const field = inject(formFieldKey, null)

const controlId = computed(() => typeof attrs.id === 'string' ? attrs.id : field?.controlId.value)
const invalid = computed(() => Boolean(
  field?.invalid.value || attrs['aria-invalid'] === true || attrs['aria-invalid'] === 'true',
))
const describedBy = computed(() => [
  typeof attrs['aria-describedby'] === 'string' ? attrs['aria-describedby'] : undefined,
  field?.describedBy.value,
].filter(Boolean).join(' ') || undefined)
const rootAttrs = computed(() => ({ class: attrs.class, style: attrs.style }))
const controlAttrs = computed(() => Object.fromEntries(
  Object.entries(attrs).filter(([key]) => ![
    'class',
    'style',
    'id',
    'aria-describedby',
    'aria-invalid',
    'aria-required',
  ].includes(key)),
))

const sizeClasses: Record<TextareaSize, string> = {
  sm: 'px-3 py-2.5 text-xs',
  md: 'px-3.5 py-3 text-[13px]',
  lg: 'px-4 py-3.5 text-[14px]',
}

const textareaClasses = computed(() => [
  'cp-scrollbar w-full resize-none rounded-cp-control border-0 bg-[var(--cp-input-current-bg,var(--cp-input-context-bg))] text-cp-primary shadow-cp-input outline-none transition-[background-color,box-shadow,color] duration-160 placeholder:text-cp-muted-text motion-reduce:transition-none',
  'hover:bg-[var(--cp-input-current-bg-hover,var(--cp-input-context-bg-hover))] hover:shadow-cp-input-hover focus:bg-(--cp-input-soft-bg-focus) focus:shadow-cp-input-focus',
  'disabled:cursor-not-allowed disabled:bg-cp-disabled disabled:text-cp-disabled-text disabled:shadow-none',
  'leading-[1.55] font-emphasis',
  sizeClasses[props.size],
  invalid.value ? 'bg-(--cp-input-error-soft-bg) shadow-cp-input-error' : undefined,
])
</script>

<template>
  <div v-bind="rootAttrs" class="min-w-0">
    <textarea
      v-bind="controlAttrs"
      :id="controlId"
      v-model="model"
      :class="textareaClasses"
      :rows="rows"
      :placeholder="placeholder"
      :disabled="disabled"
      :required="field?.required.value || undefined"
      :aria-describedby="describedBy"
      :aria-invalid="invalid || undefined"
      :aria-required="field?.required.value || undefined"
    />
  </div>
</template>
