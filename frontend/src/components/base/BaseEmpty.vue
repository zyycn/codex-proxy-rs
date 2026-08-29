<script setup lang="ts">
import type { Component } from 'vue'
import { Inbox } from '@lucide/vue'
import { computed } from 'vue'

type EmptySurface = 'subtle' | 'inset' | 'none'

const props = withDefaults(
  defineProps<{
    title?: string
    description?: string
    icon?: Component
    size?: 'sm' | 'md'
    surface?: EmptySurface
  }>(),
  {
    title: undefined,
    description: undefined,
    icon: undefined,
    size: 'md',
    surface: 'subtle',
  },
)

const resolvedTitle = computed(() => props.title ?? '暂无数据')
const resolvedIcon = computed(() => props.icon ?? Inbox)

const surfaceClasses: Record<EmptySurface, string> = {
  subtle: 'rounded-cp-lg bg-cp-fill-alter',
  inset: 'rounded-cp-lg bg-cp-fill-alter/70',
  none: 'bg-transparent',
}
</script>

<template>
  <div
    class="grid justify-items-center text-center"
    :class="[
      surfaceClasses[surface],
      size === 'sm' ? 'gap-2 px-4 py-5' : 'gap-3 px-6 py-8',
    ]"
  >
    <span
      class="inline-flex items-center justify-center rounded-cp bg-cp-fill-tertiary text-cp-text-quaternary"
      :class="size === 'sm' ? 'size-8' : 'size-10'"
    >
      <component :is="resolvedIcon" :size="size === 'sm' ? 16 : 18" />
    </span>
    <p class="m-0 text-cp leading-[1.15] font-heavy text-cp-text">
      {{ resolvedTitle }}
    </p>
    <p
      v-if="description"
      class="m-0 max-w-72 text-xs leading-[1.45] font-semibold text-cp-text-secondary"
    >
      {{ description }}
    </p>
    <div v-if="$slots.action" class="mt-1">
      <slot name="action" />
    </div>
  </div>
</template>
