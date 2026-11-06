<script setup lang="ts">
import type { OutboundProxyRecord } from '@/api'

import { ZButton, ZCard, ZCheckbox, ZConfirmDialog, ZInput, ZPageHeader, ZPagination, ZTable, ZTag } from '@codex-proxy/ui'
import { LockKeyhole, Plus, Search, Users, Wifi } from '@lucide/vue'
import { shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { usePageSelection } from '@/composables/usePageSelection'
import ProxyAccountsModal from './components/ProxyAccountsModal.vue'
import ProxyActions from './components/ProxyActions.vue'
import ProxyFormModal from './components/ProxyFormModal.vue'
import ProxyRegion from './components/ProxyRegion.vue'
import { useProxies } from './composables/useProxies'
import { useProxyBatchTesting } from './composables/useProxyBatchTesting'
import { proxyColumns } from './constants'

const { t } = useI18n({ useScope: 'global' })

const {
  search,
  proxies,
  loading,
  pagination,
  loadProxies,
  reloadAfterCreate,
  showForm,
  editing,
  form,
  formTestResult,
  saving,
  showDelete,
  pendingDelete,
  deleting,
  testingIds,
  updatingIds,
  showDisable,
  pendingDisable,
  disabling,
  toggleEnabled,
  confirmDisable,
  testingConnection,
  detectingLocation,
  openForm,
  checkProxy,
  testSavedProxies,
  testConnection,
  detectLocation,
  save,
  requestDelete,
  confirmDelete,
  setPage,
  setPageSize,
  clearCredentials,
} = useProxies()
const showAccounts = shallowRef(false)
const inspected = shallowRef<OutboundProxyRecord | null>(null)
const { selectedIds, selectedRowKeys, allSelected, indeterminate, toggleSelection, toggleAll } = usePageSelection(proxies)
const { busy: batchTesting, start: testSelectedProxies } = useProxyBatchTesting({
  test: testSavedProxies,
  reload: () => loadProxies({ silent: true }),
})

watch(search, () => {
  selectedIds.value = new Set()
})
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <ZPageHeader
      :title="t('navigation.proxies')"
      :description="t('proxies.page.description')"
    />
    <ZCard class="mt-5 flex h-[calc(100dvh-136px)] min-h-125 flex-col">
      <template #header>
        <div class="flex w-full flex-col gap-3 sm:flex-row sm:items-center">
          <ZInput v-model="search" :disabled="batchTesting" class="sm:w-80" :aria-label="t('proxies.page.searchLabel')" :placeholder="t('proxies.page.searchPlaceholder')">
            <template #prefix>
              <Search class="size-4.5 text-z-text-tertiary" />
            </template>
          </ZInput>
          <div class="flex shrink-0 flex-wrap items-center justify-end gap-2 sm:ml-auto">
            <ZButton v-if="selectedIds.size" :loading="batchTesting" :disabled="loading" :aria-label="t('proxies.page.batchTestAction')" @click="testSelectedProxies(selectedRowKeys)">
              <Wifi class="size-4" />
              {{ t('proxies.page.batchTest', { count: selectedIds.size }) }}
            </ZButton>
            <ZButton type="primary" @click="openForm()">
              <template #icon>
                <Plus class="size-4" />
              </template>
              {{ t('proxies.editor.createTitle') }}
            </ZButton>
          </div>
        </div>
      </template>
      <template #body>
        <div class="flex h-full min-h-0 flex-col">
          <ZTable class="min-h-0 flex-1" :columns="proxyColumns" :data="proxies" :loading="loading" :selected-row-keys="selectedRowKeys" :empty-text="search.trim() ? t('proxies.page.noMatches') : t('proxies.page.empty')">
            <template #header-selection>
              <ZCheckbox :model-value="allSelected" :indeterminate="indeterminate" :disabled="batchTesting" :aria-label="t('proxies.page.selectCurrentPageProxy')" @update:model-value="toggleAll" />
            </template>
            <template #selection="{ row }">
              <ZCheckbox :model-value="selectedIds.has(row.id)" :disabled="batchTesting" :aria-label="t('proxies.page.selectProxy', { name: row.name })" @update:model-value="toggleSelection(row.id)" />
            </template>
            <template #name="{ row }">
              <span class="block truncate text-z text-z-text" :title="row.name">{{ row.name }}</span>
            </template>
            <template #address="{ row }">
              <span class="flex min-w-0 items-center gap-1 text-z-xs font-emphasis text-z-text-quaternary">
                <LockKeyhole v-if="row.hasAuthentication" class="size-3 shrink-0" :aria-label="t('proxies.page.credentialsSaved')" />
                <span class="truncate font-mono" :title="row.endpoint">{{ row.endpoint }}</span>
              </span>
            </template>
            <template #status="{ row }">
              <ZTag :type="row.enabled ? 'success' : 'default'">
                {{ row.enabled ? t('common.status.enabled') : t('common.status.disabled') }}
              </ZTag>
            </template>
            <template #exitIp="{ row }">
              <div v-if="row.lastTest?.exitIpv4 && row.lastTest?.exitIpv6" class="flex flex-col gap-0.5 font-mono text-z-xs">
                <span class="truncate" :title="`IPv4: ${row.lastTest.exitIpv4}`">
                  {{ row.lastTest.exitIpv4 }}
                </span>
                <span class="truncate" :title="`IPv6: ${row.lastTest.exitIpv6}`">
                  {{ row.lastTest.exitIpv6 }}
                </span>
              </div>
              <div v-else-if="row.lastTest?.exitIpv4" class="truncate font-mono text-z-xs" :title="`IPv4: ${row.lastTest.exitIpv4}`">
                {{ row.lastTest.exitIpv4 }}
              </div>
              <div v-else-if="row.lastTest?.exitIpv6" class="truncate font-mono text-z-xs" :title="`IPv6: ${row.lastTest.exitIpv6}`">
                {{ row.lastTest.exitIpv6 }}
              </div>
              <span v-else-if="row.lastTest?.exitIp" class="block truncate font-mono text-z-xs" :title="row.lastTest.exitIp">{{ row.lastTest.exitIp }}</span>
              <span v-else class="text-z-text-quaternary">-</span>
            </template>
            <template #region="{ row }">
              <ProxyRegion :proxy="row" class="text-z-sm" />
            </template>
            <template #latency="{ row }">
              <span v-if="testingIds.has(row.id)" class="text-z-text-secondary">{{ t('proxies.page.underTest') }}</span>
              <span v-else-if="row.lastTest?.success" class="whitespace-nowrap tabular-nums text-z-success">
                {{ row.lastTest.latencyMs }} ms
              </span>
              <span v-else-if="row.lastTest" class="text-z-error" :title="t('proxies.page.ms', { message: row.lastTest.message, latencyMs: row.lastTest.latencyMs })">{{ t('common.status.failed') }}</span>
              <span v-else class="text-z-text-quaternary">{{ t('proxies.page.notTested') }}</span>
            </template>
            <template #accounts="{ row }">
              <ZButton variant="link" size="small" class="h-auto! w-fit rounded-sm! p-0! text-z-sm! leading-normal! font-normal! no-underline! hover:text-z-primary-text! [&>span]:gap-1.5" :aria-label="t('proxies.page.viewAccountsAssociatedWith', { name: row.name, count: row.accountCount })" @click="inspected = row; showAccounts = true">
                <Users class="size-3.5" aria-hidden="true" />
                <span class="font-mono tabular-nums">{{ row.accountCount }}</span>
              </ZButton>
            </template>
            <template #testedAt="{ row }">
              {{ row.lastTestAtDisplay ?? '-' }}
            </template>
            <template #actions="{ row }">
              <ProxyActions :proxy="row" :testing="testingIds.has(row.id)" :batch-testing="batchTesting" :updating="updatingIds.has(row.id)" @test="checkProxy" @edit="openForm" @toggle="toggleEnabled" @delete="requestDelete" />
            </template>
          </ZTable>
          <ZPagination v-bind="pagination" :disabled="loading" @current-change="setPage" @size-change="setPageSize" />
        </div>
      </template>
    </ZCard>

    <ProxyFormModal
      v-model="showForm"
      v-model:name="form.name"
      v-model:proxy-url="form.proxyUrl"
      v-model:max-websocket-connections="form.maxWebsocketConnections"
      v-model:custom-location="form.customLocation"
      v-model:location="form.location"
      :test-result="formTestResult"
      :proxy="editing"
      :saving="saving"
      :testing-connection="testingConnection"
      :detecting-location="detectingLocation"
      :test-saved-proxies="testSavedProxies"
      :reload-after-create="reloadAfterCreate"
      @save="save"
      @test="testConnection"
      @detect-location="detectLocation"
      @after-leave="clearCredentials"
    />
    <ZConfirmDialog v-model="showDisable" :title="t('proxies.actions.disableProxy')" :confirm-text="t('proxies.actions.disableProxy')" :loading="disabling" @confirm="confirmDisable">
      <p class="m-0">
        {{ t('proxies.page.disable', { name: pendingDisable?.name }) }}
      </p>
      <p class="mt-3 mb-0 text-z-text-secondary">
        {{ t('proxies.page.disableDescription') }}
      </p>
    </ZConfirmDialog>
    <ZConfirmDialog v-model="showDelete" :title="t('proxies.actions.deleteProxy')" :confirm-text="t('proxies.actions.deleteProxy')" destructive :loading="deleting" @confirm="confirmDelete">
      <p class="m-0">
        {{ t('proxies.page.delete', { name: pendingDelete?.name }) }}
      </p>
      <p class="mt-3 mb-0 text-z-text-secondary">
        {{ t('proxies.page.deleteDescription') }}
      </p>
    </ZConfirmDialog>
    <ProxyAccountsModal v-model="showAccounts" :proxy="inspected" @changed="loadProxies({ silent: true })" />
  </div>
</template>
