<script setup lang="ts">
defineProps<{
  title: string
  description?: string
}>()
</script>

<template>
  <header class="flex min-h-17 shrink-0 items-start justify-between gap-4">
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
    <div v-if="$slots.actions" class="mt-0.5 flex shrink-0 items-center gap-2">
      <slot name="actions" />
    </div>
  </header>
</template>
