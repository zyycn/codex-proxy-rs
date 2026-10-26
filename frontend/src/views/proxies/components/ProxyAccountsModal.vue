<script setup lang="ts">
import type { OutboundProxyAccount, OutboundProxyRecord } from '@/api'

import { defineTableColumns, ZConfirmDialog, ZDialog, ZIconButton, ZInput, ZPagination, ZTable } from '@codex-proxy/ui'
import { Search, Unlink } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import AccountGroupMarks from '@/components/AccountGroupMarks.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { stablePresetVisualToneClass } from '@/utils/color'
import { useProxyAccounts } from '../composables/useProxyAccounts'

const props = defineProps<{
  proxy: OutboundProxyRecord | null
}>()

const emit = defineEmits<{ removed: [] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const pendingRemove = shallowRef<OutboundProxyAccount | null>(null)
const showRemove = shallowRef(false)
const removeProxyName = shallowRef('')
const { accounts, loading, error, pagination, search, setPage, setPageSize, removing, removeAccount } = useProxyAccounts({
  isOpen: () => open.value,
  proxyId: () => props.proxy?.id,
  onRemoved: () => emit('removed'),
})
const columns = computed(() => (defineTableColumns<OutboundProxyAccount>([
  {
    key: 'identity',
    label: t('accounts.account'),
    kind: 'identity',
    size: '3xl',
  },
  {
    key: 'provider',
    label: t('proxies.accounts.platformType'),
    kind: 'custom',
    size: 'md',
    align: 'center',
  },
  {
    key: 'plan',
    label: t('proxies.accounts.subscription'),
    kind: 'custom',
    size: 'sm',
    align: 'center',
  },
  {
    key: 'groups',
    label: t('groups.group'),
    kind: 'custom',
    size: 'lg',
    align: 'center',
  },
  {
    key: 'actions',
    label: t('common.fields.actions'),
    kind: 'actions',
    size: 'xs',
    align: 'center',
  },
])))
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
  || (search.value.trim()
    ? t('proxies.accounts.noMatches')
    : t('proxies.accounts.empty')))
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

watch([
  open,
  () => props.proxy?.id,
], () => {
  showRemove.value = false
})
</script>

<template>
  <ZDialog v-model="open" :title="t('proxies.accounts.linkAccount')" :description="description" width="48rem" :show-close="!removing" :close-on-click-modal="!removing" :close-on-press-escape="!removing">
    <div class="flex min-h-0 flex-col">
      <ZInput v-model="search" class="mb-4 shrink-0 sm:w-80" :disabled="removing" :aria-label="t('proxies.accounts.searchAssociatedAccounts')" :placeholder="t('proxies.accounts.searchAccountNameOrEmail')">
        <template #prefix>
          <Search class="size-4.5 text-cp-text-tertiary" />
        </template>
      </ZInput>
      <ZTable :key="proxy?.id" class="min-h-0 shrink-0 [--cp-table-row-height:64px]" :style="{ height: tableHeight }" :columns="columns" :data="rows" :loading="loading" :empty-text="emptyText">
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
          <ZIconButton
            size="small"
            :aria-label="t('proxies.accounts.removeAction')"
            :disabled="removing"
            :loading="removing && pendingRemove?.id === row.id"
            @click="requestRemove(row)"
          >
            <Unlink class="size-3.5 text-cp-error" />
          </ZIconButton>
        </template>
      </ZTable>
      <ZPagination v-bind="pagination" :disabled="loading || removing" @current-change="setPage" @size-change="setPageSize" />
    </div>
  </ZDialog>
  <ZConfirmDialog v-model="showRemove" :title="t('proxies.accounts.removeAssociatedAccount')" :confirm-text="t('proxies.accounts.remove')" :loading="removing" @confirm="confirmRemove">
    <p class="m-0 wrap-break-word">
      {{ t('proxies.accounts.removeDescription', { name: pendingRemove?.name, removeProxyName }) }}
    </p>
  </ZConfirmDialog>
</template>
