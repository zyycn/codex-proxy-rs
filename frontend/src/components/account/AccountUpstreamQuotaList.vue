<script setup lang="ts">
import type { UpstreamQuotaAccount } from '@/api'
import { ZButton, ZDialog } from '@codex-proxy/ui'
import { shallowRef, watch } from 'vue'
import AccountQuotaEntry from './AccountQuotaEntry.vue'

const props = defineProps<{ accounts: UpstreamQuotaAccount[] }>()
const modelAccount = shallowRef<UpstreamQuotaAccount | null>(null)
const modelsOpen = shallowRef(false)

function showModels(account: UpstreamQuotaAccount) {
  modelAccount.value = account
  modelsOpen.value = true
}

watch(() => props.accounts, () => {
  modelsOpen.value = false
})
</script>

<template>
  <div class="grid min-w-0 gap-4">
    <section v-for="entry in accounts" :key="entry.id" class="grid min-w-0 gap-2 rounded-cp bg-cp-fill-quaternary p-3">
      <div class="flex min-w-0 flex-wrap items-baseline justify-between gap-x-2 gap-y-1">
        <div class="flex min-w-0 items-baseline gap-2">
          <h4 class="m-0 truncate text-cp-xs font-heavy text-cp-text" :title="entry.label">
            {{ entry.label }}
          </h4>
          <ZButton v-if="entry.models?.length" type="primary" variant="text" size="small" class="h-auto shrink-0 px-1.5 py-0.5 text-cp-xs" aria-haspopup="dialog" @click="showModels(entry)">
            模型详情
          </ZButton>
          <span v-if="entry.status === 'stale'" class="shrink-0 text-cp-xs text-cp-warning-text">数据过期</span>
        </div>
        <span v-if="entry.observedAt" class="shrink-0 text-cp-xs whitespace-nowrap text-cp-text-quaternary">上游额度最后获取时间：{{ entry.observedAtDisplay }}</span>
      </div>
      <AccountQuotaEntry v-if="entry.windows.length" :windows="entry.windows" :label="null" remaining />
      <span v-else class="text-cp-xs text-cp-text-tertiary">额度待观测</span>
    </section>
    <ZDialog v-if="modelAccount" v-model="modelsOpen" title="账号模型" :description="modelAccount.label" width="36rem">
      <ul class="m-0 grid list-none gap-2 p-0 sm:grid-cols-2">
        <li v-for="model in modelAccount.models" :key="model" class="min-w-0 rounded-cp bg-cp-fill-quaternary px-3 py-2 font-mono text-cp-xs break-all text-cp-text">
          {{ model }}
        </li>
      </ul>
      <template #footer>
        <ZButton @click="modelsOpen = false">
          关闭
        </ZButton>
      </template>
    </ZDialog>
  </div>
</template>
