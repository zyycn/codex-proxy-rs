<script setup lang="ts">
const props = withDefaults(defineProps<{
  as?: keyof HTMLElementTagNameMap
  title?: string
  description?: string
  padded?: boolean
  radiusClass?: string
}>(), {
  as: 'section',
  title: undefined,
  description: undefined,
  padded: true,
  radiusClass: 'rounded-[var(--cp-card-radius)]',
})
</script>

<template>
  <component
    :is="props.as"
    class="overflow-hidden bg-[var(--cp-bg-surface)] shadow-[var(--cp-shadow-card)]"
    :class="[props.radiusClass, props.padded ? 'p-6 md:px-7' : undefined]"
  >
    <header v-if="props.title || props.description || $slots.actions" class="mb-6 flex items-start justify-between gap-5">
      <div class="min-w-0">
        <h2 v-if="props.title" class="m-0 text-xl font-[760] leading-[1.15] text-[var(--cp-text-primary)]">
          {{ props.title }}
        </h2>
        <p v-if="props.description" class="mt-[7px] mb-0 text-[13px] font-semibold leading-[1.15] text-[var(--cp-text-secondary)]">
          {{ props.description }}
        </p>
      </div>
      <div v-if="$slots.actions" class="shrink-0">
        <slot name="actions" />
      </div>
    </header>
    <slot />
  </component>
</template>
