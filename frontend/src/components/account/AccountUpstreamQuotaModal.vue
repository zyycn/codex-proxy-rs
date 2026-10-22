<script setup lang="ts">
import type { AccountUpstreamQuota } from '@/api'
import { ZButton, ZDialog, ZEmpty, ZSkeleton } from '@codex-proxy/ui'
import { shallowRef, watch } from 'vue'
import { getAccountQuota } from '@/api'
import { useRequestState } from '@/composables/useRequestState'
import AccountUpstreamQuotaList from './AccountUpstreamQuotaList.vue'

const props = defineProps<{ accountId: string }>()
const open = defineModel<boolean>({ required: true })
const quota = shallowRef<AccountUpstreamQuota | null>(null)
const request = useRequestState()

async function load() {
  const requestId = request.start()
  try {
    const result = await getAccountQuota({ accountId: props.accountId }, { signal: request.signal, silent: true })
    if (request.isCurrent(requestId))
      quota.value = result.account.quota.upstream ?? null
  }
  catch (error) {
    request.fail(requestId, error)
  }
  finally {
    request.finish(requestId)
  }
}

watch([open, () => props.accountId], ([isOpen]) => {
  request.invalidate({ resetLoading: isOpen })
  if (!isOpen)
    return
  quota.value = null
  void load()
}, { immediate: true })
</script>

<template>
  <ZDialog v-model="open" title="上游账号额度" width="40rem">
    <div class="grid min-w-0 gap-3 pb-1">
      <div v-if="request.loading.value" role="status" aria-busy="true" class="grid gap-4">
        <span class="sr-only">正在读取上游额度</span>
        <ZSkeleton v-for="index in 3" :key="index" class="h-24 rounded-cp" />
      </div>
      <ZEmpty v-else-if="request.error.value" title="额度读取失败" :description="request.error.value" surface="none">
        <template #action>
          <ZButton size="small" @click="load">
            重新读取
          </ZButton>
        </template>
      </ZEmpty>
      <template v-else>
        <AccountUpstreamQuotaList v-if="quota?.accounts.length" :accounts="quota.accounts" />
        <ZEmpty v-else title="额度待观测" surface="none" />
        <p v-if="quota?.lastErrorDisplay" role="status" class="m-0 text-cp-xs leading-relaxed text-cp-warning-text">
          {{ quota.lastErrorDisplay }}
          <span v-if="quota.fetchedAt"> · 最近获取：{{ quota.fetchedAtDisplay }}</span>
        </p>
      </template>
    </div>
    <template #footer>
      <ZButton @click="open = false">
        关闭
      </ZButton>
    </template>
  </ZDialog>
</template>
