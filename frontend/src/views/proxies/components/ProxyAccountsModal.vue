<script setup lang="ts">
import type { OutboundProxyAccount, OutboundProxyRecord } from '@/api'
import { BaseConfirmModal, BaseIconButton, BaseInput, BaseModal, BaseTable, BaseTablePagination, defineTableColumns } from '@codex-proxy/ui'
import { Search, Unlink } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import AccountGroupMarks from '@/components/AccountGroupMarks.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { stablePresetVisualToneClass } from '@/utils/color'
import { useProxyAccounts } from '../composables/useProxyAccounts'

const props = defineProps<{
  proxy: OutboundProxyRecord | null
}>()
const emit = defineEmits<{ removed: [] }>()
const open = defineModel<boolean>({ required: true })
const pendingRemove = shallowRef<OutboundProxyAccount | null>(null)
const showRemove = shallowRef(false)
const removeProxyName = shallowRef('')
const { accounts, loading, error, pagination, search, setPage, setPageSize, removing, removeAccount } = useProxyAccounts({
  isOpen: () => open.value,
  proxyId: () => props.proxy?.id,
  onRemoved: () => emit('removed'),
})
const columns = defineTableColumns<OutboundProxyAccount>([
  { key: 'identity', label: '账号', kind: 'identity', size: '3xl' },
  { key: 'provider', label: '平台/类型', kind: 'custom', size: 'md', align: 'center' },
  { key: 'plan', label: '订阅', kind: 'custom', size: 'sm', align: 'center' },
  { key: 'groups', label: '分组', kind: 'custom', size: 'lg', align: 'center' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'xs', align: 'center' },
])
const rows = computed(() => accounts.value.map(account => ({
  ...account,
  displayName: account.email && account.name === account.email
    ? account.email.split('@')[0]
    : account.name,
  initial: Array.from(account.name.trim())[0]?.toUpperCase() || '—',
  avatarTone: stablePresetVisualToneClass(account.id),
})))
const description = computed(() => props.proxy
  ? props.proxy.name
  : undefined)
const emptyText = computed(() => error.value
  || (search.value.trim() ? '没有找到匹配的账号，请尝试其他名称或邮箱' : '暂无关联账号'))
const tableHeight = computed(() => {
  // 空列表与少量账号保留最小高度，多行限制高度并在表格内滚动。
  const count = accounts.value.length || (loading.value
    ? Math.min(pagination.value.total || props.proxy?.accountCount || 0, pagination.value.pageSize)
    : 0)
  return `min(55dvh, max(20rem, min(30rem, ${44 + count * 64}px)))`
})

async function confirmRemove() {
  const account = pendingRemove.value
  if (showRemove.value && account && await removeAccount(account.id))
    showRemove.value = false
}

function requestRemove(account: OutboundProxyAccount) {
  pendingRemove.value = account
  removeProxyName.value = props.proxy?.name ?? ''
  showRemove.value = true
}

watch([open, () => props.proxy?.id], () => {
  showRemove.value = false
})
</script>

<template>
  <BaseModal v-model="open" title="关联账号" :description="description" size="lg" :dismissible="!removing">
    <div class="flex min-h-0 flex-col">
      <BaseInput v-model="search" class="mb-4 shrink-0 sm:w-80" :disabled="removing" aria-label="搜索关联账号" placeholder="搜索账号名称或邮箱...">
        <template #prefix>
          <Search class="size-4.5 text-cp-text-tertiary" />
        </template>
      </BaseInput>
      <BaseTable :key="proxy?.id" class="min-h-0 shrink-0 [--cp-table-row-height:64px]" :style="{ height: tableHeight }" :columns="columns" :rows="rows" :loading="loading" :empty-text="emptyText">
        <template #identity="{ row }">
          <div class="flex min-w-0 items-center gap-3">
            <span class="inline-flex size-9 shrink-0 items-center justify-center rounded-lg text-cp font-extrabold" :class="row.avatarTone" aria-hidden="true">
              {{ row.initial }}
            </span>
            <div class="min-w-0 flex-1">
              <div class="truncate text-cp font-heavy text-cp-text" :title="row.name">
                {{ row.displayName }}
              </div>
              <div v-if="row.email" class="mt-0.5 truncate font-mono text-cp-xs font-emphasis text-cp-text-quaternary" :title="row.email">
                {{ row.email }}
              </div>
            </div>
          </div>
        </template>
        <template #provider="{ row }">
          <ProviderIconGroup :provider="row.provider" :authentication-kind="row.authenticationKind" />
        </template>
        <template #plan="{ row }">
          <AccountPlanBadge v-if="row.planType && row.planTypeDisplay" :plan-type="row.planType" :plan-type-display="row.planTypeDisplay" />
          <span v-else class="text-cp-xs text-cp-text-quaternary">—</span>
        </template>
        <template #groups="{ row }">
          <div class="flex justify-center">
            <AccountGroupMarks :groups="row.groups || []" />
          </div>
        </template>
        <template #actions="{ row }">
          <BaseIconButton
            size="sm"
            label="从当前代理移除账号"
            :disabled="removing"
            :loading="removing && pendingRemove?.id === row.id"
            @click="requestRemove(row)"
          >
            <Unlink class="size-3.5 text-cp-error" />
          </BaseIconButton>
        </template>
      </BaseTable>
      <BaseTablePagination :pagination="pagination" :loading="loading || removing" @page-change="setPage" @page-size-change="setPageSize" />
    </div>
  </BaseModal>
  <BaseConfirmModal v-model="showRemove" title="移除关联账号" confirm-text="移除" :loading="removing" @confirm="confirmRemove">
    <p class="m-0 wrap-break-word">
      将“{{ pendingRemove?.name }}”从“{{ removeProxyName }}”移除后，该账号将改为直连
    </p>
  </BaseConfirmModal>
</template>
