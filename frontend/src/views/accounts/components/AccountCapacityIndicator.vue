<script setup lang="ts">
import type { Account } from '@/api'
import { ZButton } from '@codex-proxy/ui'
import { Grid2X2, Slash } from '@lucide/vue'
import { computed } from 'vue'

const props = defineProps<{
  capacity: Account['capacity']
}>()

defineEmits<{ showDetails: [] }>()

const full = computed(() => props.capacity.usedSlots !== null
  && props.capacity.totalSlots !== null
  && props.capacity.usedSlots >= props.capacity.totalSlots)
const label = computed(() => `账号并发占用 ${props.capacity.usedSlots ?? '未知'}，上限 ${props.capacity.totalSlots ?? '不限'}`)
</script>

<template>
  <ZButton
    variant="text" size="small" :type="full ? 'warning' : 'default'"
    :aria-label="`${label}，查看并发明细`"
    :title="`${label}，查看明细`"
    class="relative top-0.75 -m-0.5 h-auto! px-0.5! py-0.5! whitespace-nowrap font-mono text-[9px]! font-normal! tabular-nums hover:bg-cp-bg-text-hover!"
    @click.stop="$emit('showDetails')"
  >
    <span aria-hidden="true" class="inline-flex items-center gap-1">
      <Grid2X2 class="size-2.5 shrink-0" :stroke-width="1.8" />
      <!-- 数字按字面高度居中，分隔线使用图标以避免字形基线偏移 -->
      <span class="inline-flex items-center gap-0.5">
        <span class="[text-box:trim-both_cap_alphabetic]">{{ capacity.usedSlots ?? '—' }}</span>
        <Slash class="size-1.75 shrink-0 -rotate-20" />
        <span class="[text-box:trim-both_cap_alphabetic]">{{ capacity.totalSlots ?? '∞' }}</span>
      </span>
    </span>
  </ZButton>
</template>
