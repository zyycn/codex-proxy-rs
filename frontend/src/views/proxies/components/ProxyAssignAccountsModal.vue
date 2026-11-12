<script setup lang="ts">
import type { Account, OutboundProxyRecord } from '@/api'
import { defineTableColumns, ZButton, ZCheckbox, ZDialog, ZInput, ZPagination, ZTable } from '@codex-proxy/ui'
import { Search } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import AccountGroupMarks from '@/components/AccountGroupMarks.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { useProxyAccountAssignment } from '../composables/useProxyAccountAssignment'

const props = defineProps<{ proxy: OutboundProxyRecord | null }>()
const emit = defineEmits<{ assigned: [] }>()
const open = defineModel<boolean>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const {
  accounts,
  loading,
  error,
  search,
  pagination,
  setPage,
  setPageSize,
  selectedIds,
  selectedRowKeys,
  allSelected,
  indeterminate,
  toggleSelection,
  toggleAll,
  tooManySelected,
  maximumAccounts,
  assigning,
  assign,
} = useProxyAccountAssignment({
  isOpen: () => open.value,
  proxy: () => props.proxy,
  onAssigned: () => emit('assigned'),
})
const columns = computed(() => defineTableColumns<Account>([
  {
    key: 'selection',
    kind: 'selection',
    size: 'xs',
    align: 'center',
  },
  {
    key: 'identity',
    label: t('accounts.account'),
    kind: 'identity',
    size: '2xl',
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
    key: 'outboundProxy',
    label: t('proxies.assignment.currentProxy'),
    kind: 'custom',
    size: '2xl',
  },
]))
const emptyText = computed(() => error.value || (search.value.trim()
  ? t('proxies.accounts.noMatches')
  : t('proxies.assignment.noAccounts')))

async function confirmAssignment() {
  if (await assign())
    open.value = false
}
</script>

<template>
  <ZDialog v-model="open" :title="t('proxies.assignment.title')" :description="proxy?.name" width="64rem" :show-close="!assigning" :close-on-click-modal="!assigning" :close-on-press-escape="!assigning">
    <div class="flex min-h-0 flex-col gap-3">
      <div class="flex min-w-0 items-center gap-2">
        <ZInput v-model="search" class="min-w-0 flex-1 sm:w-80 sm:flex-none" :disabled="assigning" :aria-label="t('proxies.assignment.searchAccounts')" :placeholder="t('proxies.accounts.searchAccountNameOrEmail')">
          <template #prefix>
            <Search class="size-4.5 text-z-text-tertiary" />
          </template>
        </ZInput>
        <div v-if="selectedIds.size" class="ml-auto flex shrink-0 gap-2">
          <ZButton type="danger" variant="plain" :disabled="assigning" @click="selectedIds = new Set()">
            {{ t('common.actions.reset') }}
          </ZButton>
          <ZButton type="primary" :loading="assigning" :disabled="!proxy?.enabled || tooManySelected" @click="confirmAssignment">
            {{ t('proxies.assignment.assignAction', { count: selectedIds.size }) }}
          </ZButton>
        </div>
      </div>
      <ZTable class="min-h-0 shrink-0 [--z-table-row-height:56px]" :style="{ height: 'min(50dvh, 28rem)' }" :columns="columns" :data="accounts" :loading="loading" :empty-text="emptyText" :selected-row-keys="selectedRowKeys">
        <template #header-selection>
          <ZCheckbox :model-value="allSelected" :indeterminate="indeterminate" :disabled="loading || assigning" :aria-label="t('proxies.assignment.selectPage')" @update:model-value="toggleAll" />
        </template>
        <template #selection="{ row }">
          <ZCheckbox :model-value="selectedIds.has(row.id)" :disabled="loading || assigning || row.outboundProxyId === proxy?.id" :aria-label="t('proxies.assignment.selectAccount', { name: row.name })" @update:model-value="toggleSelection(row.id)" />
        </template>
        <template #identity="{ row }">
          <div class="min-w-0">
            <div class="truncate text-z font-heavy text-z-text" :title="row.name">
              {{ row.name }}
            </div>
            <div v-if="row.email && row.email !== row.name" class="mt-0.5 truncate font-mono text-z-xs text-z-text-secondary" :title="row.email">
              {{ row.email }}
            </div>
          </div>
        </template>
        <template #provider="{ row }">
          <ProviderIconGroup :provider="row.provider" :authentication-kind="row.authenticationKind" />
        </template>
        <template #plan="{ row }">
          <AccountPlanBadge :upstream="row.upstream" :authentication-kind="row.authenticationKind" :plan-type="row.planType" :plan-type-display="row.planTypeDisplay || '—'" />
        </template>
        <template #groups="{ row }">
          <div class="flex justify-center">
            <AccountGroupMarks :groups="row.groups" />
          </div>
        </template>
        <template #outboundProxy="{ row }">
          <span v-if="row.outboundProxyId === proxy?.id" class="text-z-success-text">{{ t('proxies.assignment.alreadyAssigned') }}</span>
          <span v-else class="block truncate text-z-xs text-z-text-secondary" :title="row.outboundProxyEndpoint ?? undefined">
            {{ row.outboundProxyEndpoint || t('accounts.proxy.deploymentDefault') }}
          </span>
        </template>
      </ZTable>
      <p v-if="tooManySelected" class="m-0 text-z-sm text-z-error-text" role="alert">
        {{ t('proxies.assignment.selectionLimit', { count: maximumAccounts }) }}
      </p>
      <p v-if="proxy && !proxy.enabled" class="m-0 text-z-sm text-z-warning-text" role="alert">
        {{ t('proxies.assignment.enableFirst') }}
      </p>
    </div>
    <template #footer>
      <ZPagination v-bind="pagination" class="mt-0! w-full" :disabled="loading || assigning" @current-change="setPage" @size-change="setPageSize" />
    </template>
  </ZDialog>
</template>
