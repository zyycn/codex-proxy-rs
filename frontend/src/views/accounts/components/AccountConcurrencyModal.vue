<script setup lang="ts">
import type { Account } from '@/api'
import { ZDialog, ZIconButton, ZPagination } from '@codex-proxy/ui'
import { Grid2X2, RefreshCw } from '@lucide/vue'
import { toRef } from 'vue'
import { useAccountConcurrency } from '../composables/useAccountConcurrency'
import AccountConcurrencyTable from './AccountConcurrencyTable.vue'

const props = defineProps<{ account: Account | null }>()
const open = defineModel<boolean>({ required: true })
const { pageSize, data, entries, loading, tableLoading, error, refresh, setPage, setPageSize } = useAccountConcurrency(toRef(props, 'account'), open)
</script>

<template>
  <ZDialog v-model="open" title="并发明细" :description="`${account?.name ?? '当前账号'} · 当前正在处理的请求`" width="72rem">
    <div class="grid min-w-0 gap-2">
      <div class="flex items-center justify-between gap-3">
        <div class="flex items-center gap-1.5 text-cp-sm" role="group" aria-label="当前占用 / 并发上限">
          <Grid2X2 class="size-3.5 text-cp-text-tertiary" :stroke-width="1.8" aria-hidden="true" />
          <span class="whitespace-nowrap font-mono tabular-nums">
            <strong class="font-emphasis text-cp-text">{{ data?.total ?? account?.capacity.usedSlots ?? '—' }}</strong>
            <span class="text-cp-text-tertiary"> / {{ account?.capacity.totalSlots ?? '不限' }}</span>
          </span>
        </div>
        <ZIconButton
          aria-label="刷新并发明细" title="每 5 秒自动刷新" variant="solid" size="small"
          :disabled="loading" :aria-busy="loading" @click="refresh"
        >
          <RefreshCw class="size-4" :class="{ 'animate-spin motion-reduce:animate-none': loading }" />
        </ZIconButton>
      </div>
      <AccountConcurrencyTable
        :entries="entries" :total="data?.total ?? 0" :page-size="data?.pageSize ?? pageSize"
        :loading="loading && !data" :aria-busy="loading"
        :empty-text="error ? '暂时无法读取并发明细' : '当前没有并发占用'"
      />
    </div>
    <template v-if="data && data.total > 0" #footer>
      <ZPagination
        class="-mt-3! w-full"
        :current-page="data.page" :page-size="data.pageSize" :total="data.total" :page-sizes="[20, 50, 100]"
        :disabled="tableLoading" @current-change="setPage" @size-change="setPageSize"
      />
    </template>
  </ZDialog>
</template>
