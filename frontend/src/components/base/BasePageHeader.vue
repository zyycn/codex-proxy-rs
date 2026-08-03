<script setup lang="ts">
const props = defineProps<{
  title: string
  description?: string
  actionsInline?: boolean
}>()
</script>

<template>
  <header
    class="grid min-h-17 shrink-0 items-start"
    :class="
      props.actionsInline
        ? 'grid-cols-[minmax(0,1fr)_auto] gap-x-4'
        : 'grid-cols-1 gap-y-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:gap-x-4'
    "
  >
    <div class="min-w-0">
      <h1 class="m-0 text-[34px] leading-[1.15] font-extrabold text-(--cp-text-primary)">
        {{ title }}
      </h1>
      <p
        v-if="description || $slots.description"
        class="mt-2.5 mb-0 flex items-center gap-2 text-[15px] leading-[1.15] font-semibold text-(--cp-text-secondary)"
      >
        <slot name="description">
          {{ description }}
        </slot>
      </p>
    </div>
    <div
      v-if="$slots.actions"
      class="mt-0.5 flex shrink-0 items-center justify-end gap-2"
      :class="props.actionsInline ? undefined : 'max-sm:w-full'"
    >
      <slot name="actions" />
    </div>
  </header>
</template>
