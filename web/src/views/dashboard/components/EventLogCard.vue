<script setup lang="ts">
import { computed, ref } from 'vue'
import BaseCard from '../../../components/base/BaseCard.vue'
import BaseEmpty from '../../../components/base/BaseEmpty.vue'
import BaseSegmented from '../../../components/base/BaseSegmented.vue'
import BaseTable from '../../../components/base/BaseTable/index.vue'

const props = defineProps<{
  rows: any[]
}>()

const activeFilter = ref('all')
const filters = [
  { label: '全部', value: 'all' },
  { label: '错误', value: 'error' },
]
const eventLogColumns = [
  {
    key: 'time',
    label: '时间',
    width: 86,
    fixed: 'left' as const,
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums',
  },
  { key: 'level', label: '级别' },
  {
    key: 'requestId',
    label: '请求 ID',
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums',
  },
  { key: 'route', label: '路由' },
  { key: 'model', label: '模型' },
  {
    key: 'statusCode',
    label: '状态码',
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums',
  },
  {
    key: 'latency',
    label: '延迟',
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums',
  },
]

const levelToneClasses: Record<string, string> = {
  normal: 'bg-(--cp-normal-bg) text-(--cp-normal-text)',
  info: 'bg-(--cp-info-bg) text-(--cp-info-text)',
  success: 'bg-(--cp-success-bg) text-(--cp-success-text)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning-text)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
}

const statusToneClasses: Record<string, string> = {
  normal: 'text-(--cp-normal-text)',
  info: 'text-(--cp-success-text)',
  success: 'text-(--cp-success-text)',
  warning: 'text-(--cp-warning-text)',
  danger: 'text-(--cp-danger-text)',
}

function rowTone(row: object) {
  return (row as any).tone
}

const filteredRows = computed(() => {
  if (activeFilter.value === 'error') return props.rows.filter((row) => row.tone === 'danger')
  return props.rows
})
</script>

<template>
  <BaseCard
    as="article"
    variant="dashboard"
    title="事件日志"
    description="最近 50 条事件"
    class="h-87.5 w-full"
  >
    <template #actions>
      <BaseSegmented v-model="activeFilter" :options="filters" class="w-38" />
    </template>

    <template #body>
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
          min-width="760px"
        >
          <template #level="{ row, value }">
            <span
              class="inline-flex h-6 w-14.5 items-center justify-center rounded-full text-xs leading-[1.15] font-bold"
              :class="levelToneClasses[rowTone(row)]"
              >{{ value }}</span
            >
          </template>

          <template #statusCode="{ row, value }">
            <span class="font-bold" :class="statusToneClasses[rowTone(row)]">{{ value }}</span>
          </template>
        </BaseTable>
      </div>
    </template>
  </BaseCard>
</template>
