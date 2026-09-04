import type { Ref } from 'vue'
import type { UsageDisplayRecord } from '../utils/records'
import type { UsageTimeRangeParams } from './useUsageTimeRange'
import { watchDebounced } from '@vueuse/core'

import { computed, onMounted, onScopeDispose, shallowRef, watch } from 'vue'
import {
  getUsageRecordInsightsDiagnostics,
  getUsageRecordInsightsOverview,
  getUsageRecords,
  getUsageRecordSummary,
} from '@/api'
import { toast } from '@/components/base/BaseToast'

import { errorMessage, withMinimumDuration } from '@/utils/async'

interface UseUsageRecordsTableOptions {
  timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>
  latestTimeRangeParams: () => UsageTimeRangeParams
}

type UsageLoadScope = 'all' | 'table'

const CLIENT_API_KEY_VISIBLE_PREFIX_LENGTH = 10

interface UsageLoadOptions {
  scope?: UsageLoadScope
  background?: boolean
}

export function useUsageRecordsTable(options: UseUsageRecordsTableOptions) {
  const loading = shallowRef(true)
  const analyticsLoading = shallowRef(true)
  const records = shallowRef<UsageDisplayRecord[]>([])
  const summary = shallowRef(emptySummary())
  const insights = shallowRef(emptyInsights())
  const currentPage = shallowRef(1)
  const pageSize = shallowRef(10)
  const totalRecords = shallowRef(0)
  const searchQuery = shallowRef('')
  const providerQuery = shallowRef('')
  const tableTimeRangeParams = shallowRef<UsageTimeRangeParams>({
    ...options.timeRangeParams.value,
  })
  const refreshingList = shallowRef(false)
  const diagnosticDimension = shallowRef('model')
  let tableRequestId = 0
  let analyticsRequestId = 0
  let diagnosticRequestId = 0
  const scopedParams = () => ({
    ...options.timeRangeParams.value,
    ...(providerQuery.value ? { provider: providerQuery.value } : {}),
  })
  const filterParams = () => ({
    search: usageSearchParam(searchQuery.value),
  })
  const usagePagination = computed(() => ({
    currentPage: currentPage.value,
    pageSize: pageSize.value,
    total: totalRecords.value,
  }))

  function resetPagination() {
    currentPage.value = 1
    totalRecords.value = 0
  }

  async function loadUsageRecords(loadOptions: UsageLoadOptions = {}) {
    const { scope = 'all', background = false } = loadOptions
    const globalParams = scopedParams()
    if (scope === 'all') {
      resetPagination()
      tableTimeRangeParams.value = { ...globalParams }
    }

    await Promise.all([
      loadUsagePage(background),
      ...(scope === 'all' ? [loadUsageAnalytics(globalParams, background)] : []),
    ])
  }

  async function loadUsagePage(background: boolean) {
    const requestId = ++tableRequestId
    loading.value = !background
    try {
      const result = await getUsageRecords({
        currentPage: currentPage.value,
        pageSize: pageSize.value,
        ...tableTimeRangeParams.value,
        ...filterParams(),
      })
      if (requestId !== tableRequestId)
        return

      records.value = result.items
      pageSize.value = result.pageSize
      totalRecords.value = result.total
      currentPage.value = result.currentPage
    }
    catch (error: unknown) {
      if (requestId !== tableRequestId)
        return
      toast.error(errorMessage(error, '加载失败'))
    }
    finally {
      if (requestId === tableRequestId) {
        loading.value = false
      }
    }
  }

  async function loadUsageAnalytics(globalParams: ReturnType<typeof scopedParams>, background: boolean) {
    const requestId = ++analyticsRequestId
    const diagnosticsId = ++diagnosticRequestId
    const dimension = diagnosticDimension.value
    analyticsLoading.value = !background
    try {
      const [nextSummary, overview, diagnostics] = await Promise.all([
        getUsageRecordSummary(globalParams),
        getUsageRecordInsightsOverview(globalParams),
        getUsageRecordInsightsDiagnostics({
          ...globalParams,
          dimension,
        }),
      ])
      if (requestId !== analyticsRequestId)
        return

      summary.value = nextSummary
      insights.value = {
        overview,
        diagnostics:
          diagnosticsId === diagnosticRequestId && dimension === diagnosticDimension.value
            ? diagnostics
            : insights.value.diagnostics,
      }
    }
    catch (error: unknown) {
      if (requestId !== analyticsRequestId)
        return
      toast.error(errorMessage(error, '加载失败'))
    }
    finally {
      if (requestId === analyticsRequestId) {
        analyticsLoading.value = false
      }
    }
  }

  async function loadDiagnostics() {
    const requestId = ++diagnosticRequestId
    const dimension = diagnosticDimension.value
    const params = scopedParams()
    try {
      const diagnostics = await getUsageRecordInsightsDiagnostics({
        ...params,
        dimension,
      })
      if (requestId !== diagnosticRequestId || dimension !== diagnosticDimension.value)
        return
      insights.value = {
        ...insights.value,
        diagnostics,
      }
    }
    catch (error: unknown) {
      if (requestId !== diagnosticRequestId || dimension !== diagnosticDimension.value)
        return
      toast.error(errorMessage(error, '加载失败'))
    }
  }

  async function refreshUsageRecords() {
    if (refreshingList.value || loading.value)
      return
    refreshingList.value = true
    try {
      tableTimeRangeParams.value = options.latestTimeRangeParams()
      resetPagination()
      await withMinimumDuration(() => loadUsageRecords({ scope: 'table' }))
    }
    finally {
      refreshingList.value = false
    }
  }

  function handlePageChange(nextPage: number) {
    currentPage.value = nextPage
    void loadUsageRecords({ scope: 'table' })
  }

  function handlePageSizeChange(nextPageSize: number) {
    pageSize.value = nextPageSize
    resetPagination()
    void loadUsageRecords({ scope: 'table' })
  }

  onMounted(() => {
    loadUsageRecords()
  })

  watch(diagnosticDimension, () => {
    void loadDiagnostics()
  })

  watch(providerQuery, () => {
    void loadUsageRecords({ background: true })
  })

  watchDebounced(
    searchQuery,
    () => {
      resetPagination()
      void loadUsageRecords({ scope: 'table' })
    },
    { debounce: 250 },
  )

  onScopeDispose(() => {
    tableRequestId += 1
    analyticsRequestId += 1
    diagnosticRequestId += 1
  })

  return {
    currentPage,
    pageSize,
    searchQuery,
    providerQuery,
    usagePagination,
    loading,
    analyticsLoading,
    records,
    summary,
    insights,
    refreshingList,
    diagnosticDimension,
    loadUsageRecords,
    refreshUsageRecords,
    handlePageChange,
    handlePageSizeChange,
  }
}

function usageSearchParam(value: string) {
  const search = value.trim()
  if (!search)
    return undefined
  if (search.startsWith('sk_'))
    return search.slice(0, CLIENT_API_KEY_VISIBLE_PREFIX_LENGTH)
  return search
}

function emptySummary() {
  const summary: Awaited<ReturnType<typeof getUsageRecordSummary>> = {
    totalRequests: '0',
    inputTokens: '0',
    outputTokens: '0',
    cachedTokens: '0',
    cacheWriteTokens: '0',
    totalTokens: '0',
    averageLatencyMs: '0 ms',
  }
  return summary
}

function emptyInsights() {
  return {
    overview: emptyOverview(),
    diagnostics: emptyDiagnostics(),
  }
}

function emptyOverview() {
  const overview: Awaited<ReturnType<typeof getUsageRecordInsightsOverview>> = {
    granularity: '1d',
    health: {
      totalRequests: 0,
      successRequests: 0,
      failedRequests: 0,
      cancelledRequests: 0,
      incompleteRequests: 0,
      callerErrorRequests: 0,
      successRate: 0,
      completionRate: 0,
      requestChangeRate: null,
      successRateChange: null,
      points: [],
    },
    performance: {
      latencyP50Ms: null,
      latencyP95Ms: null,
      latencyP99Ms: null,
      firstTokenP50Ms: null,
      firstTokenP95Ms: null,
      firstTokenP99Ms: null,
      admissionDecisionP50Ms: null,
      admissionDecisionP95Ms: null,
      accountSelectionWaitP50Ms: null,
      accountSelectionWaitP95Ms: null,
      outputThroughputP10: null,
      outputThroughputP50: null,
      outputThroughputP90: null,
      capacityUtilization: null,
      capacityUtilizationP95: null,
      latencyCoverage: 0,
      firstTokenCoverage: 0,
      admissionDecisionCoverage: 0,
      accountSelectionWaitCoverage: 0,
      capacityCoverage: 0,
      points: [],
    },
    cost: {
      estimatedCost: null,
      standardCost: null,
      noCacheCost: null,
      cacheSavings: null,
      tierPremium: null,
      costPerRequest: null,
      costPerSuccessfulRequest: null,
      tokensPerRequest: 0,
      cachedTokenRate: 0,
      cacheHitRequestRate: 0,
      inputTokens: 0,
      outputTokens: 0,
      cachedTokens: 0,
      totalTokens: 0,
      points: [],
      coverage: { known: 0, partial: 0, unknown: 0, notBillable: 0 },
    },
  }
  return overview
}

function emptyDiagnostics() {
  const diagnostics: Awaited<ReturnType<typeof getUsageRecordInsightsDiagnostics>> = {
    dimension: 'model',
    items: [],
  }
  return diagnostics
}
