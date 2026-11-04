<script setup lang="ts">
import type { Account, OutboundProxyRecord } from '@/api'
import { defineTableColumns, ZButton, ZCheckbox, ZDialog, ZInput, ZPagination, ZTable } from '@codex-proxy/ui'
import { Search } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
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
  <ZDialog v-model="open" :title="t('proxies.assignment.title')" :description="proxy?.name" width="48rem" :show-close="!assigning" :close-on-click-modal="!assigning" :close-on-press-escape="!assigning">
    <div class="flex min-h-0 flex-col gap-3">
      <ZInput v-model="search" class="shrink-0 sm:w-80" :disabled="assigning" :aria-label="t('proxies.assignment.searchAccounts')" :placeholder="t('proxies.accounts.searchAccountNameOrEmail')">
        <template #prefix>
          <Search class="size-4.5 text-z-text-tertiary" />
        </template>
      </ZInput>
      <p class="m-0 text-z-sm text-z-text-secondary">
        {{ t('proxies.assignment.replaceHelp') }}
      </p>
      <ZTable class="min-h-0 shrink-0 [--z-table-row-height:64px]" :style="{ height: 'min(50dvh, 28rem)' }" :columns="columns" :data="accounts" :loading="loading" :empty-text="emptyText" :selected-row-keys="selectedRowKeys">
        <template #header-selection>
          <ZCheckbox :model-value="allSelected" :indeterminate="indeterminate" :disabled="loading || assigning" :aria-label="t('proxies.assignment.selectPage')" @update:model-value="toggleAll" />
        </template>
        <template #selection="{ row }">
          <ZCheckbox :model-value="row.outboundProxyId === proxy?.id || selectedIds.has(row.id)" :disabled="loading || assigning || row.outboundProxyId === proxy?.id" :aria-label="t('proxies.assignment.selectAccount', { name: row.name })" @update:model-value="toggleSelection(row.id)" />
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
        <template #outboundProxy="{ row }">
          <span v-if="row.outboundProxyId === proxy?.id" class="text-z-success-text">{{ t('proxies.assignment.alreadyAssigned') }}</span>
          <span v-else class="block truncate text-z-xs text-z-text-secondary" :title="row.outboundProxyEndpoint ?? undefined">
            {{ row.outboundProxyEndpoint || t('accounts.proxy.directConnection') }}
          </span>
        </template>
      </ZTable>
      <ZPagination v-bind="pagination" :disabled="loading || assigning" @current-change="setPage" @size-change="setPageSize" />
      <p v-if="tooManySelected" class="m-0 text-z-sm text-z-error-text" role="alert">
        {{ t('proxies.assignment.selectionLimit', { count: maximumAccounts }) }}
      </p>
      <p v-if="proxy && !proxy.enabled" class="m-0 text-z-sm text-z-warning-text" role="alert">
        {{ t('proxies.assignment.enableFirst') }}
      </p>
    </div>
    <template #footer>
      <span class="mr-auto text-z-sm text-z-text-secondary">{{ t('proxies.assignment.selectedCount', { count: selectedIds.size }) }}</span>
      <ZButton :disabled="assigning" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton type="primary" :loading="assigning" :disabled="!proxy?.enabled || !selectedIds.size || tooManySelected" @click="confirmAssignment">
        {{ t('proxies.assignment.assignAction') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
