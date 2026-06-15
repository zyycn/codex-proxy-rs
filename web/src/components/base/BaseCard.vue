<script setup lang="ts">
withDefaults(defineProps<{
  title?: string
  description?: string
  padded?: boolean
}>(), {
  title: undefined,
  description: undefined,
  padded: true,
})
</script>

<template>
  <section
    class="overflow-hidden rounded-[var(--cp-card-radius)] bg-[var(--cp-bg-surface)] shadow-[var(--cp-shadow-card)]"
    :class="{ 'p-6 md:px-7': padded }"
  >
    <header v-if="title || description || $slots.actions" class="mb-6 flex items-start justify-between gap-5">
      <div class="min-w-0">
        <h2 v-if="title" class="m-0 text-xl font-[760] leading-[1.15] text-[var(--cp-text-primary)]">
          {{ title }}
        </h2>
        <p v-if="description" class="mt-[7px] mb-0 text-[13px] font-semibold leading-[1.15] text-[var(--cp-text-secondary)]">
          {{ description }}
        </p>
      </div>
      <div v-if="$slots.actions" class="shrink-0">
        <slot name="actions" />
      </div>
    </header>
    <slot />
  </section>
</template>
