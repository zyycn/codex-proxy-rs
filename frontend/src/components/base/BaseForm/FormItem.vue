<script setup lang="ts">
withDefaults(
  defineProps<{
    label?: string
    description?: string
    error?: string
    required?: boolean
  }>(),
  {
    label: undefined,
    description: undefined,
    error: undefined,
    required: false,
  },
)
</script>

<template>
  <div class="min-w-0">
    <div
      v-if="label || $slots.label || $slots.extra"
      class="mb-2 flex min-w-0 items-center justify-between gap-3"
    >
      <div class="flex min-w-0 items-center gap-1.5">
        <slot name="label">
          <label class="min-w-0 text-[13px] leading-none font-medium text-(--cp-text-secondary)">
            {{ label }}
          </label>
        </slot>
        <span v-if="required" class="text-[13px] leading-none font-bold text-(--cp-danger)">
          *
        </span>
      </div>
      <div v-if="$slots.extra" class="inline-flex shrink-0 items-center">
        <slot name="extra" />
      </div>
    </div>
    <p
      v-if="description"
      class="mt-0 mb-2 text-xs leading-[1.35] font-[650] text-(--cp-text-muted)"
    >
      {{ description }}
    </p>
    <div class="min-w-0">
      <slot />
    </div>
    <p v-if="error" class="mt-2 mb-0 text-xs leading-[1.35] font-[650] text-(--cp-danger-text)">
      {{ error }}
    </p>
  </div>
</template>
