<script setup lang="ts">
import { computed, ref } from 'vue'
import BaseCard from '../../../components/base/BaseCard.vue'
import BaseEmpty from '../../../components/base/BaseEmpty.vue'
import BaseTable from '../../../components/base/BaseTable.vue'
import type { EventLogItem, SemanticTone } from '../types'

const props = defineProps<{
  rows: EventLogItem[]
}>()

const activeFilter = ref('all')
const filters = [
  { label: '全部', value: 'all' },
  { label: '警告', value: 'warn' },
  { label: '错误', value: 'error' },
]
const eventLogColumns = [
  { key: 'time', label: '时间', width: '11.5%' },
  { key: 'level', label: '级别', width: '7.5%' },
  { key: 'requestId', label: '请求 ID', width: '17%' },
  { key: 'route', label: '路由', width: '18.5%' },
  { key: 'model', label: '模型', width: '13%' },
  { key: 'statusCode', label: '状态码', width: '8.5%' },
  { key: 'latency', label: '延迟', width: '24%' },
]

const levelToneClasses: Record<SemanticTone, string> = {
  normal: 'bg-(--cp-normal-bg) text-(--cp-normal-text)',
  info: 'bg-(--cp-info-bg) text-(--cp-info-text)',
  success: 'bg-(--cp-success-bg) text-(--cp-success-text)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning-text)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
}

const statusToneClasses: Record<SemanticTone, string> = {
  normal: 'text-(--cp-normal-text)',
  info: 'text-(--cp-success-text)',
  success: 'text-(--cp-success-text)',
  warning: 'text-(--cp-warning-text)',
  danger: 'text-(--cp-danger-text)',
}

function rowTone(row: object) {
  return (row as EventLogItem).tone
}

const filteredRows = computed(() => {
  if (activeFilter.value === 'warn') return props.rows.filter((row) => row.tone === 'warning')
  if (activeFilter.value === 'error') return props.rows.filter((row) => row.tone === 'danger')
  return props.rows
})
</script>

<template>
  <BaseCard as="article" :padded="false" class="h-87.5 w-full px-7 pt-6">
    <header class="flex items-start justify-between">
      <div>
        <h2 class="m-0 text-xl leading-[1.15] font-[760] text-(--cp-text-primary)">事件日志</h2>
        <p class="mt-1.75 mb-0 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)">
          最近 50 条事件
        </p>
      </div>

      <div
        class="grid h-9 w-51.5 grid-cols-[62px_58px_58px] gap-1 rounded-xl bg-(--cp-bg-muted) p-1"
      >
        <button
          v-for="filter in filters"
          :key="filter.value"
          class="h-7 rounded-[9px] border-0 text-xs leading-[1.15] font-[650] cursor-pointer"
          :class="
            activeFilter === filter.value
              ? 'bg-white text-(--cp-text-primary) shadow-(--cp-shadow-control)'
              : 'bg-transparent text-(--cp-text-secondary)'
          "
          type="button"
          @click="activeFilter = filter.value"
        >
          {{ filter.label }}
        </button>
      </div>
    </header>

    <div class="mt-4.25 flex h-60 w-full justify-between overflow-hidden">
      <BaseEmpty
        v-if="filteredRows.length === 0"
        compact
        class="w-full place-content-center"
        :title="rows.length === 0 ? '暂无事件日志' : '没有匹配日志'"
        :description="
          rows.length === 0 ? '请求经过代理后会在这里显示最近事件。' : '调整筛选条件后再查看。'
        "
      />
      <BaseTable
        v-else
        class="min-w-0 flex-1"
        :columns="eventLogColumns"
        :rows="filteredRows"
        table-class="min-w-full"
        header-row-class="h-10 rounded-xl bg-(--cp-bg-subtle) text-xs leading-[1.15] font-bold text-(--cp-text-secondary)"
        body-row-class="h-14 rounded-[10px] transition-colors duration-200 hover:bg-(--cp-bg-subtle)"
        header-cell-class="min-w-0 overflow-hidden bg-(--cp-bg-subtle) px-5 text-ellipsis whitespace-nowrap first:rounded-l-xl last:rounded-r-xl"
        body-cell-class="min-w-0 overflow-hidden px-5 text-ellipsis whitespace-nowrap text-xs leading-[1.15] font-[650] text-(--cp-text-primary) first:rounded-l-[10px] last:rounded-r-[10px]"
      >
        <template #time="{ value }">
          <span class="font-mono tabular-nums">{{ value }}</span>
        </template>

        <template #level="{ row, value }">
          <span
            class="inline-flex h-6 w-14.5 items-center justify-center rounded-full text-xs leading-[1.15] font-bold"
            :class="levelToneClasses[rowTone(row)]"
            >{{ value }}</span
          >
        </template>

        <template #requestId="{ value }">
          <span class="font-mono tabular-nums">{{ value }}</span>
        </template>

        <template #statusCode="{ row, value }">
          <span class="font-mono font-bold tabular-nums" :class="statusToneClasses[rowTone(row)]">{{
            value
          }}</span>
        </template>

        <template #latency="{ value }">
          <span class="font-mono tabular-nums">{{ value }}</span>
        </template>
      </BaseTable>
    </div>
  </BaseCard>
</template>
