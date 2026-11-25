<script setup lang="ts">
import type { UsageListRecord } from '@/api'

import { ZCard } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { usageRecordColumns } from '@/components/usage-records-table/columns'
import UsageRecordsTable from '@/components/usage-records-table/index.vue'

defineProps<{
  rows: UsageListRecord[]
}>()

const { t } = useI18n({ useScope: 'global' })

const dashboardUsageRecordColumns = computed(() => usageRecordColumns.value.filter(
  column => column.key !== 'actions' && column.key !== 'clientApiKeyName',
))
</script>

<template>
  <ZCard
    as="article"
    :title="t('dashboard.recentRequests.title')"
    :description="t('dashboard.recentRequests.description')"
    class="h-117 w-full"
  >
    <template #body>
      <div class="flex h-91 w-full overflow-hidden">
        <UsageRecordsTable
          class="min-w-0 flex-1"
          :columns="dashboardUsageRecordColumns"
          :rows="rows"
          :empty-text="t('dashboard.recentRequests.empty')"
        />
      </div>
    </template>
  </ZCard>
</template>
