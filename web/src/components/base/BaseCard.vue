<script setup lang="ts">
const props = withDefaults(
  defineProps<{
    as?: keyof HTMLElementTagNameMap
    padded?: boolean
    radiusClass?: string
    shadowClass?: string
  }>(),
  {
    as: 'section',
    padded: true,
    radiusClass: 'rounded-(--cp-card-radius)',
    shadowClass: 'shadow-(--cp-shadow-card)',
  },
)
</script>

<template>
  <component
    :is="props.as"
    class="[--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] overflow-hidden bg-(--cp-bg-surface)"
    :class="[props.radiusClass, props.shadowClass, props.padded ? 'p-6 md:px-7' : undefined]"
  >
    <slot />
  </component>
</template>
