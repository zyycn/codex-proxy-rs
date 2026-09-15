import type { Ref } from 'vue'

import type { UsageTimeRangeParams } from './useUsageTimeRange'
import { useAuthStore } from '@/stores/modules/auth'

import {
  keyDiagnosticDimensionOptions,
  keyUsageRecordColumns,
  usageRecordColumns,
} from '@/views/usage/model/columns'
import { useAdminUsageSource } from './useAdminUsageSource'
import { useKeyUsageSource } from './useKeyUsageSource'

interface UseUsageOptions {
  timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>
  latestTimeRangeParams: () => UsageTimeRangeParams
  active: Readonly<Ref<boolean>>
}

/** 页面只消费统一状态，身份差异收敛在数据源和字段配置。 */
export function useUsage(options: UseUsageOptions) {
  const authStore = useAuthStore()
  if (authStore.isKey) {
    return {
      ...useKeyUsageSource(options),
      isAdmin: false as const,
      columns: keyUsageRecordColumns,
      diagnosticDimensionOptions: keyDiagnosticDimensionOptions,
      searchPlaceholder: '搜索模型',
      searchAriaLabel: '按模型搜索使用记录',
    }
  }

  return {
    ...useAdminUsageSource(options),
    isAdmin: true as const,
    columns: usageRecordColumns,
    diagnosticDimensionOptions: undefined,
    searchPlaceholder: '请求、密钥名称、账号或模型',
    searchAriaLabel: '搜索使用记录：请求、密钥名称、账号或模型',
  }
}
