<script setup lang="ts">
const props = withDefaults(
  defineProps<{
    as?: keyof HTMLElementTagNameMap
    title?: string
    description?: string
    padded?: boolean
    radiusClass?: string
    shadowClass?: string
  }>(),
  {
    as: 'section',
    title: undefined,
    description: undefined,
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
    <header
      v-if="props.title || props.description || $slots.actions"
      class="mb-6 flex items-start justify-between gap-5"
    >
      <div class="min-w-0">
        <h2
          v-if="props.title"
          class="m-0 text-xl font-[760] leading-[1.15] text-(--cp-text-primary)"
        >
          {{ props.title }}
        </h2>
        <p
          v-if="props.description"
          class="mt-1.75 mb-0 text-[13px] font-semibold leading-[1.15] text-(--cp-text-secondary)"
        >
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
