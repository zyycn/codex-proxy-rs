<script setup lang="ts">
import { computed, provide, useId } from 'vue'
import { formFieldKey } from './context'

const props = withDefaults(
  defineProps<{
    label?: string
    description?: string
    error?: string
    required?: boolean
    controlId?: string
  }>(),
  {
    label: undefined,
    description: undefined,
    error: undefined,
    required: false,
    controlId: undefined,
  },
)

const generatedId = useId()
const resolvedControlId = computed(() => props.controlId ?? `field-${generatedId}`)
const descriptionId = computed(() => props.description ? `${resolvedControlId.value}-description` : undefined)
const errorId = computed(() => props.error ? `${resolvedControlId.value}-error` : undefined)
const describedBy = computed(() => [descriptionId.value, errorId.value].filter(Boolean).join(' ') || undefined)

provide(formFieldKey, {
  controlId: resolvedControlId,
  describedBy,
  invalid: computed(() => Boolean(props.error)),
  required: computed(() => props.required),
})
</script>

<template>
  <div class="min-w-0">
    <div
      v-if="label || $slots.label || $slots.extra"
      class="mb-2 flex min-w-0 items-center justify-between gap-3"
    >
      <!-- The runtime id comes from the injected field context. -->
      <!-- eslint-disable-next-line vue-a11y/label-has-for -->
      <label
        :for="resolvedControlId"
        class="flex min-w-0 items-center gap-1.5 text-[13px] leading-none font-medium text-cp-secondary"
      >
        <slot name="label">{{ label }}</slot>
        <span v-if="required" class="font-bold text-cp-danger" aria-hidden="true">*</span>
        <span v-if="required" class="sr-only">必填</span>
      </label>
      <div v-if="$slots.extra" class="inline-flex shrink-0 items-center">
        <slot name="extra" />
      </div>
    </div>
    <p
      v-if="description"
      :id="descriptionId"
      class="mt-0 mb-2 text-xs leading-[1.35] font-emphasis text-cp-muted-text"
    >
      {{ description }}
    </p>
    <div class="min-w-0">
      <slot />
    </div>
    <p
      v-if="error"
      :id="errorId"
      class="mt-2 mb-0 text-xs leading-[1.35] font-emphasis text-cp-danger-text"
      aria-live="polite"
    >
      {{ error }}
    </p>
  </div>
</template>
