<script setup lang="ts">
import { computed } from 'vue'
import type { Component } from 'vue'
import { Inbox } from '@lucide/vue'

const props = defineProps<{
  title?: string
  message?: string
  description?: string
  icon?: Component
  compact?: boolean
  plain?: boolean
}>()

const resolvedTitle = computed(() => props.title ?? props.message ?? '暂无数据')
const resolvedIcon = computed(() => props.icon ?? Inbox)
</script>

<template>
  <div
    class="grid justify-items-center text-center"
    :class="[
      plain ? 'bg-transparent' : 'rounded-(--cp-panel-radius) bg-(--cp-bg-subtle)',
      compact ? 'gap-2 px-4 py-5' : 'gap-3 px-6 py-8',
    ]"
  >
    <span
      class="inline-flex items-center justify-center rounded-(--cp-icon-button-radius) bg-(--cp-bg-muted) text-(--cp-text-muted)"
      :class="compact ? 'size-8' : 'size-10'"
    >
      <component :is="resolvedIcon" :size="compact ? 16 : 18" />
    </span>
    <p class="m-0 text-[13px] leading-[1.15] font-[760] text-(--cp-text-primary)">
      {{ resolvedTitle }}
    </p>
    <p
      v-if="description"
      class="m-0 max-w-72 text-xs leading-[1.45] font-semibold text-(--cp-text-secondary)"
    >
      {{ description }}
    </p>
    <div v-if="$slots.action" class="mt-1">
      <slot name="action" />
    </div>
  </div>
</template>
