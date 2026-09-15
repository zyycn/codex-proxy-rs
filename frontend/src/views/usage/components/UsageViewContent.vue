<script setup lang="ts">
import type {
  UsageDiagnosticsResponse,
  UsageInsightsOverviewResponse,
  UsageSummaryResponse,
} from '@/api'
import type { BaseTableColumn } from '@/components/base/BaseTable/columns'
import type { UsageDisplayRecord } from '@/views/usage/model/records'
import BaseCard from '@/components/base/BaseCard.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import UsageRecordsTable from '@/components/business/UsageRecordsTable.vue'
import UsageFilters from './UsageFilters.vue'
import UsageInsightsGrid from './UsageInsightsGrid.vue'
import UsageSummaryCards from './UsageSummaryCards.vue'

const props = withDefaults(defineProps<{
  timeRangeOptions: Array<{ label: string, value: string }>
  summary: UsageSummaryResponse
  insights: {
    overview: UsageInsightsOverviewResponse
    diagnostics: UsageDiagnosticsResponse
  }
  columns: BaseTableColumn<UsageDisplayRecord>[]
  rows: UsageDisplayRecord[]
  pagination: { currentPage: number, pageSize: number, total: number }
  loading?: boolean
  analyticsLoading?: boolean
  refreshing?: boolean
  tableError?: string
  searchPlaceholder?: string
  searchAriaLabel?: string
  diagnosticDimensionOptions?: Array<{ label: string, value: string }>
}>(), {
  loading: false,
  analyticsLoading: false,
  refreshing: false,
  tableError: '',
  searchPlaceholder: '请求、密钥名称、账号或模型',
  searchAriaLabel: '搜索使用记录：请求、密钥名称、账号或模型',
  diagnosticDimensionOptions: undefined,
})

const emit = defineEmits<{
  refresh: []
  pageChange: [page: number]
  pageSizeChange: [pageSize: number]
}>()

const timeRange = defineModel<string>('timeRange', { required: true })
const search = defineModel<string>('search', { required: true })
const recordView = defineModel<string>('recordView', { required: true })
const diagnosticDimension = defineModel<string>('diagnosticDimension', { required: true })

const recordViewOptions = [
  { label: '成功记录', value: 'success' },
  { label: '错误排查', value: 'errors' },
]
</script>

<template>
  <div class="w-full">
    <BasePageHeader title="使用统计" description="查看请求用量、性能趋势与调用错误记录">
      <template #actions>
        <BaseSelect v-model="timeRange" :options="props.timeRangeOptions" class="w-34" />
        <slot name="scope-actions" />
      </template>
    </BasePageHeader>

    <UsageSummaryCards :summary="summary" />
    <UsageInsightsGrid
      v-model:diagnostic-dimension="diagnosticDimension"
      :overview="insights.overview"
      :diagnostics="insights.diagnostics"
      :loading="analyticsLoading"
      :diagnostic-dimension-options="diagnosticDimensionOptions"
    />

    <BaseCard class="mt-5 flex flex-col">
      <template #header>
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h2 class="m-0 text-xl leading-[1.15] font-heavy text-cp-text">
              请求明细
            </h2>
            <p class="mt-1.75 mb-0 text-cp leading-[1.15] font-emphasis text-cp-text-secondary">
              成功请求与失败请求明细
            </p>
          </div>
          <BaseSegmented v-model="recordView" label="请求明细类型" :options="recordViewOptions" class="w-52" />
        </div>
      </template>

      <template #body>
        <div
          v-show="recordView === 'success'"
          class="grid min-h-130 min-w-0 flex-1 grid-rows-[auto_minmax(0,1fr)] gap-3"
        >
          <UsageFilters
            v-model:search="search"
            :loading="loading"
            :refreshing="refreshing"
            :placeholder="searchPlaceholder"
            :aria-label="searchAriaLabel"
            @refresh="emit('refresh')"
          />

          <div class="flex min-h-0 min-w-0 flex-col">
            <p v-if="tableError && !loading" role="alert" class="text-cp-sm text-cp-error-text">
              {{ tableError }}。请刷新重试。
            </p>
            <UsageRecordsTable
              v-else
              class="min-h-0 flex-1"
              :columns="columns"
              :rows="rows"
              :loading="loading"
              empty-text="暂无使用记录"
            >
              <template v-if="$slots['record-actions']" #actions="scope">
                <slot name="record-actions" v-bind="scope" />
              </template>
            </UsageRecordsTable>
            <BaseTablePagination
              :pagination="pagination"
              :loading="loading"
              @page-change="emit('pageChange', $event)"
              @page-size-change="emit('pageSizeChange', $event)"
            />
          </div>
        </div>

        <div v-show="recordView === 'errors'" class="min-h-130 min-w-0 flex-1">
          <slot name="errors" />
        </div>
      </template>
    </BaseCard>
  </div>
</template>
