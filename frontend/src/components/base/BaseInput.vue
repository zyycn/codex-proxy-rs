<script setup lang="ts">
import { computed, inject, useAttrs } from 'vue'
import { formFieldKey } from './BaseForm/context'

type InputSize = 'sm' | 'md' | 'lg'

defineOptions({ inheritAttrs: false })

const props = withDefaults(
  defineProps<{
    placeholder?: string
    type?: string
    disabled?: boolean
    size?: InputSize
  }>(),
  {
    placeholder: '',
    type: 'text',
    disabled: false,
    size: 'md',
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

const sizeClasses: Record<InputSize, string> = {
  sm: 'h-cp-control-sm gap-2 px-3 text-xs',
  md: 'h-cp-control gap-2.5 px-3.5 text-cp',
  lg: 'h-cp-control-lg gap-3 px-4 text-cp-lg',
}

const containerClasses = computed(() => [
  'relative inline-flex w-full min-w-0 items-center overflow-visible rounded-cp border-0 text-cp-text shadow-cp-input transition-[background-color,box-shadow,color] duration-[160ms] motion-reduce:transition-none',
  sizeClasses[props.size],
  props.disabled
    ? 'cursor-not-allowed bg-cp-bg-container-disabled text-cp-text-disabled shadow-none'
    : invalid.value
      ? 'bg-(--cp-input-error-active-bg) shadow-cp-input-error-active'
      : [
          'bg-[var(--cp-input-bg)]',
          'hover:bg-[var(--cp-input-hover-bg)] hover:shadow-cp-input-hover',
          'focus-within:bg-(--cp-input-active-bg) focus-within:shadow-cp-input-active',
        ],
])

const iconClasses = computed(() => [
  'inline-flex shrink-0',
  props.disabled
    ? 'text-cp-text-disabled'
    : invalid.value
      ? 'text-cp-error'
      : 'text-cp-text-quaternary',
])

function updateModel(event: Event) {
  model.value = (event.target as HTMLInputElement).value
}
</script>

<template>
  <div v-bind="rootAttrs" class="min-w-0">
    <span class="base-input__control" :class="containerClasses">
      <span v-if="$slots.prefix" :class="iconClasses" aria-hidden="true">
        <slot name="prefix" />
      </span>
      <input
        v-bind="controlAttrs"
        :id="controlId"
        :value="model"
        class="base-input__field h-full min-w-0 flex-1 border-0 bg-transparent font-emphasis leading-[1.15] text-cp-text outline-0 placeholder:text-cp-text-quaternary disabled:cursor-not-allowed disabled:text-cp-text-disabled"
        :placeholder="placeholder"
        :type="type"
        :disabled="disabled"
        :required="field?.required.value || undefined"
        :aria-describedby="describedBy"
        :aria-invalid="invalid || undefined"
        :aria-required="field?.required.value || undefined"
        @input="updateModel"
      >
      <span v-if="$slots.suffix" :class="iconClasses">
        <slot name="suffix" />
      </span>
    </span>
  </div>
</template>

<style scoped>
.base-input__control {
  --base-input-autofill-bg: var(--cp-input-bg);
}

.base-input__control:hover {
  --base-input-autofill-bg: var(--cp-input-hover-bg);
}

.base-input__control:focus-within {
  --base-input-autofill-bg: var(--cp-input-active-bg);
}

.base-input__field:-webkit-autofill,
.base-input__field:-webkit-autofill:hover,
.base-input__field:-webkit-autofill:focus,
.base-input__field:autofill {
  caret-color: var(--cp-color-text);
  -webkit-text-fill-color: var(--cp-color-text) !important;
  -webkit-box-shadow: 0 0 0 1000px var(--base-input-autofill-bg) inset !important;
  box-shadow: 0 0 0 1000px var(--base-input-autofill-bg) inset !important;
}

/* 普通数字表单保留键盘步进，但不混入浏览器原生微调器外观。 */
.base-input__field[type='number'] {
  appearance: textfield;
}

.base-input__field[type='number']::-webkit-inner-spin-button,
.base-input__field[type='number']::-webkit-outer-spin-button {
  margin: 0;
  appearance: none;
}
</style>
