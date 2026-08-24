<script setup lang="ts">
defineProps<{
  title: string
  description?: string
}>()
</script>

<template>
  <header
    class="flex min-h-17 shrink-0 flex-wrap items-start justify-between gap-x-4 gap-y-3"
  >
    <div class="min-w-0 flex-1 basis-64">
      <h1 class="m-0 text-[34px] leading-[1.15] font-extrabold text-cp-text">
        {{ title }}
      </h1>
      <p
        v-if="description || $slots.description"
        class="mt-2.5 mb-0 flex items-center gap-2 text-cp-xl leading-[1.15] font-semibold text-cp-text-secondary"
      >
        <slot name="description">
          {{ description }}
        </slot>
      </p>
    </div>
    <div
      v-if="$slots.actions"
      class="mt-0.5 flex shrink-0 items-center justify-end gap-2"
    >
      <slot name="actions" />
    </div>
  </header>
</template>
