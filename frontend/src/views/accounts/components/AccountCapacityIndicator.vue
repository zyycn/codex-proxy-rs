<script setup lang="ts">
import type { Account } from '@/api'
import { Grid2X2, Slash } from '@lucide/vue'
import { computed } from 'vue'

const props = defineProps<{
  capacity: Account['capacity']
}>()

const full = computed(() => props.capacity.usedSlots !== null
  && props.capacity.totalSlots !== null
  && props.capacity.usedSlots >= props.capacity.totalSlots)
const label = computed(() => `账号并发占用 ${props.capacity.usedSlots ?? '未知'}，上限 ${props.capacity.totalSlots ?? '不限'}`)
</script>

<template>
  <span
    role="img"
    :aria-label="label"
    class="inline-flex shrink-0 items-center gap-1 whitespace-nowrap font-mono text-[9px] leading-none tabular-nums relative top-0.75"
    :class="full ? 'text-cp-warning' : 'text-cp-text-tertiary'"
  >
    <Grid2X2 class="size-2.5 shrink-0" :stroke-width="1.8" aria-hidden="true" />
    <!-- 数字按字面高度居中，分隔线使用图标，避免不同字形的基线影响对齐。 -->
    <span aria-hidden="true" class="inline-flex items-center gap-0.5">
      <span class="[text-box:trim-both_cap_alphabetic]">{{ capacity.usedSlots ?? '—' }}</span>
      <Slash class="size-1.75 shrink-0 -rotate-20" aria-hidden="true" />
      <span class="[text-box:trim-both_cap_alphabetic]">{{ capacity.totalSlots ?? '∞' }}</span>
    </span>
  </span>
</template>
