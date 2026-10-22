<script setup lang="ts">
import type { AccountUpstreamQuota } from '@/api'
import { ZScrollbar } from '@codex-proxy/ui'
import { ChevronDown } from '@lucide/vue'
import { shallowRef, watch } from 'vue'
import { getAccountQuota } from '@/api'
import AccountUpstreamQuotaList from '@/components/account/AccountUpstreamQuotaList.vue'
import { useRequestState } from '@/composables/useRequestState'

const props = defineProps<{
  accountId: string
  quota: AccountUpstreamQuota
}>()
const current = shallowRef<AccountUpstreamQuota | null>(props.quota)
const request = useRequestState()
watch([() => props.accountId, () => props.quota], () => {
  request.invalidate()
  request.error.value = ''
  current.value = props.quota
})

async function handleToggle(event: Event) {
  if (!(event.target as HTMLDetailsElement).open) {
    request.invalidate()
    return
  }
  const requestId = request.start()
  try {
    const result = await getAccountQuota({ accountId: props.accountId }, { signal: request.signal, silent: true })
    if (request.isCurrent(requestId))
      current.value = result.account.quota.upstream ?? null
  }
  catch (error) {
    request.fail(requestId, error)
  }
  finally {
    request.finish(requestId)
  }
}
</script>

<template>
  <div class="grid min-w-0 gap-1.5" @click.stop>
    <details v-if="current?.accounts.length" open class="group min-w-0" @toggle="handleToggle">
      <summary class="flex min-h-8 cursor-pointer list-none items-center justify-between gap-2 rounded-cp text-cp-xs font-emphasis text-cp-text-secondary outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline [&::-webkit-details-marker]:hidden">
        <span>上游账号 {{ current.accounts.length }} 个</span>
        <span class="flex shrink-0 items-center gap-1 text-cp-text-tertiary">
          <span v-if="request.loading.value">读取中</span>
          <ChevronDown class="size-3.5 transition-transform group-open:rotate-180 motion-reduce:transition-none" aria-hidden="true" />
        </span>
      </summary>
      <ZScrollbar max-height="10rem">
        <AccountUpstreamQuotaList :accounts="current.accounts" class="pt-2 pb-1" />
      </ZScrollbar>
    </details>
    <span v-else class="text-cp-xs text-cp-text-tertiary">额度待观测</span>
    <p v-if="request.error.value" role="status" class="m-0 text-cp-xs leading-relaxed text-cp-warning-text">
      额度读取失败：{{ request.error.value }}
    </p>
    <p v-if="current?.lastErrorDisplay" role="status" class="m-0 text-cp-xs leading-relaxed text-cp-warning-text">
      {{ current.lastErrorDisplay }}
      <span v-if="current.fetchedAt"> · 最近获取：{{ current.fetchedAtDisplay }}</span>
    </p>
  </div>
</template>
