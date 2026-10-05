<script setup lang="ts">
import { BaseCard, BaseCheckbox, BaseConfirmModal, BasePageHeader, BaseTable, BaseTableColumnSettings, BaseTablePagination, useTableColumns } from '@codex-proxy/ui'

import { ChevronDown } from '@lucide/vue'
import { ref } from 'vue'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import AccountGroupMarks from '@/components/AccountGroupMarks.vue'
import LastUsedAtCell from '@/components/LastUsedAtCell.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { useAccountGroupCatalog } from '@/composables/useAccountGroupCatalog'
import AccountCreateModal from './components/account-create-modal/index.vue'
import AccountImportTasks from './components/account-import-tasks/index.vue'
import AccountQuotaPanel from './components/account-quota-panel/index.vue'
import AccountQuotaSummaryCell from './components/account-quota-summary-cell/index.vue'
import AccountStatusBadge from './components/account-status-badge/index.vue'
import AccountBatchEditModal from './components/AccountBatchEditModal.vue'
import AccountConnectionTestModal from './components/AccountConnectionTestModal.vue'
import AccountEditModal from './components/AccountEditModal.vue'
import AccountFilters from './components/AccountFilters.vue'
import AccountIdentityCell from './components/AccountIdentityCell.vue'
import AccountOverviewCards from './components/AccountOverviewCards.vue'
import AccountTableActions from './components/AccountTableActions.vue'
import AccountUsagePanel from './components/AccountUsagePanel.vue'
import { useAccountBatchEditor } from './composables/useAccountBatchEditor'
import { useAccountConnectionTest } from './composables/useAccountConnectionTest'
import { useAccountEditor } from './composables/useAccountEditor'
import { useAccountImportTasks } from './composables/useAccountImportTasks'
import { useAccountMutations } from './composables/useAccountMutations'
import { useAccountsQuery } from './composables/useAccountsQuery'
import { useAccountsTable } from './composables/useAccountsTable'
import { accountColumns } from './constants'

const selectedIds = ref<Set<string>>(new Set())
const { visibleColumns, columnOptions, setColumnVisible, setColumnOrder, resetColumns } = useTableColumns(accountColumns, 'accounts')
const {
  loading,
  accounts,
  loadAccounts,
  refreshAccountsSilently,
  searchQuery,
  providerQuery,
  statusQuery,
  groupQuery,
  sort,
  accountSummary,
  accountPagination,
  replaceAccount,
  handlePageChange,
  handlePageSizeChange,
  handleSortChange,
} = useAccountsQuery()

const {
  groups,
  loading: groupsLoading,
  loadGroups,
} = useAccountGroupCatalog()

const importTasks = useAccountImportTasks({
  reload: () => Promise.all([loadAccounts(), loadGroups()]),
})
const {
  open: showImportTasks,
  tasks: recentImportTasks,
  selectedId: importTaskId,
  detail: importTaskDetail,
  loading: loadingImportTasks,
  stopping: stoppingImportTask,
  error: importTaskError,
  activeCount: activeImportCount,
} = importTasks

const {
  showCreateModal,
  showDeleteModal,
  showSingleDeleteModal,
  pendingDeleteAccount,
  deleteCount,
  recoveringAccountIds,
  refreshingAccountIds,
  refreshingQuotaAccountIds,
  downloadingCatalogAccountIds,
  togglingSchedulingAccountIds,
  deletingAccount,
  creatingAccount,
  authorizingOAuth,
  authorization,
  authorizationCallback,
  batchDeleting,
  exportingAccounts,
  exportDisabledReason,
  reauthorizingAccount,
  createForm,
  handleCreate,
  handleAuthorizeOAuth,
  openCreateAccount,
  clearCreate,
  openReauthorizeAccount,
  requestDeleteAccount,
  handleDelete,
  handleBatchDelete,
  handleExportAccounts,
  handleDownloadModelCatalog,
  handleRecover,
  handleRefresh,
  handleRefreshQuota,
  handleQuotaReset,
  handleToggleScheduling,
} = useAccountMutations({
  onImportTaskCreated: importTasks.created,
  accounts,
  selectedIds,
  reload: () => Promise.all([loadAccounts(), loadGroups()]),
  replaceAccount,
})

const {
  showConnectionTestModal,
  testingAccount,
  connectionTestStatus,
  connectionTestModel,
  connectionTestLogs,
  connectionTestError,
  connectionTestStartedAt,
  connectionTestFinishedAt,
  connectionTestDurationMs,
  testingConnectionIds,
  loadingConnectionTestModels,
  refreshingConnectionTestModels,
  connectionTestSelectedModel,
  connectionTestModelOptions,
  connectionTestStatusView,
  openConnectionTest,
  handleRefreshConnectionTestModels,
  handleTestConnection,
} = useAccountConnectionTest({ reload: refreshAccountsSilently })

const {
  expandedAccountIds,
  allSelected,
  indeterminate,
  selectedRowKeys,
  expandedRowKeys,
  toggleSelection,
  toggleExpanded,
  toggleAll,
} = useAccountsTable(accounts, selectedIds)

const {
  showBatchEditModal,
  schedulingEnabled: batchSchedulingEnabled,
  concurrencyLimit: batchConcurrencyLimit,
  weight: batchWeight,
  modelAccess: batchModelAccess,
  hasChanges: batchHasChanges,
  catalogAccountId: batchCatalogAccountId,
  editingCount: batchEditingCount,
  proxyMode: batchProxyMode,
  proxyId: batchProxyId,
  selectedGroupIds: batchGroupIds,
  saving: savingBatchEdit,
  open: openBatchEdit,
  save: saveBatchEdit,
} = useAccountBatchEditor({
  accounts,
  selectedIds,
  reloadAccounts: loadAccounts,
  reloadGroups: loadGroups,
})

const {
  apiKey: editingApiKey,
  oauthTransport: editingOAuthTransport,
  configurationLoading,
  configurationReady,
  showEditModal,
  editingAccount,
  notes: editingNotes,
  schedulingEnabled,
  concurrencyLimit: editingConcurrencyLimit,
  weight: editingWeight,
  modelAccess: editingModelAccess,
  proxyMode: editingProxyMode,
  proxyId: editingProxyId,
  selectedGroupIds: editingGroupIds,
  saving: savingAccountEdit,
  open: openAccountEdit,
  save: saveAccountEdit,
  clearCredentials,
} = useAccountEditor({
  reloadAccounts: loadAccounts,
  reloadGroups: loadGroups,
})
</script>

<template>
  <div class="flex min-h-0 w-full flex-col xl:h-full xl:overflow-hidden">
    <BasePageHeader
      class="h-17"
      title="账号管理"
      description="维护账号池，查看可用性、配额与使用状态"
    />

    <AccountOverviewCards :summary="accountSummary" />

    <BaseCard
      class="mt-4 flex flex-col xl:h-[calc(100dvh-250px)] xl:min-h-125"
    >
      <template #header>
        <AccountFilters
          v-model:search="searchQuery"
          v-model:status="statusQuery"
          v-model:provider="providerQuery"
          v-model:group="groupQuery"
          :groups="groups"
          :groups-loading="groupsLoading"
          :selected-count="selectedIds.size"
          :batch-deleting="batchDeleting"
          :exporting-accounts="exportingAccounts"
          :export-disabled-reason="exportDisabledReason"
          :has-import-tasks="recentImportTasks.length > 0"
          :active-import-count="activeImportCount"
          @import-tasks="showImportTasks = true"
          @delete-selected="showDeleteModal = true"
          @export-selected="handleExportAccounts"
          @create="openCreateAccount"
          @edit-selected="openBatchEdit"
        >
          <template #actions>
            <BaseTableColumnSettings
              :options="columnOptions"
              @change="setColumnVisible"
              @reorder="setColumnOrder"
              @reset="resetColumns"
            />
          </template>
        </AccountFilters>
      </template>

      <template #body>
        <div class="flex min-h-0 flex-col xl:h-full">
          <BaseTable
            class="h-100! min-h-100 flex-none [--cp-table-row-height:72px] xl:h-auto! xl:min-h-0 xl:flex-1"
            :columns="visibleColumns"
            :rows="accounts"
            :loading="loading"
            :selected-row-keys="selectedRowKeys"
            :expanded-row-keys="expandedRowKeys"
            :sort="sort"
            empty-text="暂无账号数据"
            @sort-change="handleSortChange"
          >
            <template #expander="{ row }">
              <button
                type="button"
                class="inline-flex size-6 cursor-pointer items-center justify-center rounded-md border-0 bg-transparent text-cp-text-secondary transition hover:bg-cp-bg-text-hover hover:text-cp-text"
                :title="expandedAccountIds.has(row.id) ? '收起统计' : '展开统计'"
                @click.stop="toggleExpanded(row.id)"
              >
                <ChevronDown
                  class="size-3.5 transition-transform"
                  :class="expandedAccountIds.has(row.id) ? '' : '-rotate-90'"
                />
              </button>
            </template>

            <template #header-selection>
              <BaseCheckbox
                :model-value="allSelected"
                :indeterminate="indeterminate"
                label="选择当前页账号"
                @update:model-value="toggleAll"
              />
            </template>

            <template #selection="{ row }">
              <BaseCheckbox
                :model-value="selectedIds.has(row.id)"
                label="选择账号"
                @update:model-value="toggleSelection(row.id)"
              />
            </template>

            <template #identity="{ row }">
              <AccountIdentityCell :account="row" show-notes />
            </template>

            <template #provider="{ row }">
              <ProviderIconGroup
                :provider="row.provider"
                :authentication-kind="row.authenticationKind"
              />
            </template>

            <template #status="{ row }">
              <AccountStatusBadge
                :status="row.status"
                :error-reason="row.errorReason"
                :error-message="row.errorMessage"
                :rate-limit-recovery-display="row.quota.rateLimitRecoveryDisplay"
                :rate-limit-reason="row.quota.rateLimitReason"
                :recovery-probe-required="row.quota.recoveryProbeRequired"
                :next-refresh-at="row.nextRefreshAt"
                :next-refresh-at-display="row.nextRefreshAtDisplay"
              />
            </template>

            <template #planType="{ row }">
              <AccountPlanBadge :authentication-kind="row.authenticationKind" :plan-type="row.planType" :plan-type-display="row.planTypeDisplay" />
            </template>

            <template #usage="{ row }">
              <AccountQuotaSummaryCell :account="row" />
            </template>

            <template #groups="{ row }">
              <div class="flex w-full justify-center">
                <AccountGroupMarks :groups="row.groups" />
              </div>
            </template>

            <template #lastUsedAt="{ row }">
              <LastUsedAtCell :value="row.usage.lastUsedAt" :display="row.usage.lastUsedAtDisplay" :full-display="row.usage.lastUsedAtFullDisplay" />
            </template>

            <template #actions="{ row }">
              <AccountTableActions
                :account="row"
                :deleting="deletingAccount"
                :downloading-catalog="downloadingCatalogAccountIds.has(row.id)"
                :recovering="recoveringAccountIds.has(row.id)"
                :refreshing="refreshingAccountIds.has(row.id)"
                :testing="testingConnectionIds.has(row.id)"
                :toggling-scheduling="togglingSchedulingAccountIds.has(row.id)"
                @edit="openAccountEdit"
                @delete="requestDeleteAccount"
                @download-model-catalog="handleDownloadModelCatalog"
                @recover="handleRecover"
                @refresh="handleRefresh"
                @reauthorize="openReauthorizeAccount"
                @test="openConnectionTest"
                @toggle-scheduling="handleToggleScheduling"
              />
            </template>

            <template #expanded="{ row }">
              <div class="grid items-stretch gap-3 p-4 lg:grid-cols-[1.05fr_2.45fr] xl:min-h-77">
                <AccountQuotaPanel
                  :account="row"
                  :refreshing="refreshingQuotaAccountIds.has(row.id)"
                  @account-updated="void replaceAccount($event)"
                  @quota-reset="handleQuotaReset"
                  @refresh-quota="handleRefreshQuota"
                />
                <AccountUsagePanel :account="row" />
              </div>
            </template>
          </BaseTable>
          <BaseTablePagination
            :pagination="accountPagination"
            :loading="loading"
            @page-change="handlePageChange"
            @page-size-change="handlePageSizeChange"
          />
        </div>
      </template>
    </BaseCard>

    <AccountConnectionTestModal
      v-model="showConnectionTestModal"
      v-model:selected-model="connectionTestSelectedModel"
      :account="testingAccount"
      :duration-ms="connectionTestDurationMs"
      :error="connectionTestError"
      :finished-at="connectionTestFinishedAt"
      :loading-models="loadingConnectionTestModels"
      :refreshing-models="refreshingConnectionTestModels"
      :logs="connectionTestLogs"
      :model="connectionTestModel"
      :model-options="connectionTestModelOptions"
      :started-at="connectionTestStartedAt"
      :status="connectionTestStatus"
      :status-view="connectionTestStatusView"
      @refresh-models="handleRefreshConnectionTestModels()"
      @test="handleTestConnection()"
    />

    <AccountImportTasks
      v-model="showImportTasks"
      :tasks="recentImportTasks"
      :selected-id="importTaskId"
      :detail="importTaskDetail"
      :loading="loadingImportTasks"
      :stopping="stoppingImportTask"
      :error="importTaskError"
      @select="importTasks.select"
      @refresh="importTasks.refresh"
      @stop="importTasks.stop"
      @after-leave="importTasks.refresh(true)"
      @view-accounts="showImportTasks = false; loadAccounts()"
    />

    <AccountCreateModal
      v-model="showCreateModal"
      v-model:form="createForm"
      v-model:callback="authorizationCallback"
      :authorization="authorization"
      :account="reauthorizingAccount"
      :groups="groups"
      :groups-loading="groupsLoading"
      :oauth-loading="authorizingOAuth"
      :reauthorizing="Boolean(reauthorizingAccount)"
      :saving="creatingAccount"
      @create="handleCreate"
      @generate-oauth="handleAuthorizeOAuth"
      @after-leave="clearCreate"
    />

    <AccountEditModal
      v-model="showEditModal"
      v-model:api-key="editingApiKey"
      v-model:oauth-transport="editingOAuthTransport"
      v-model:notes="editingNotes"
      v-model:enabled="schedulingEnabled"
      v-model:concurrency-limit="editingConcurrencyLimit"
      v-model:weight="editingWeight"
      v-model:model-access="editingModelAccess"
      v-model:proxy-mode="editingProxyMode"
      v-model:proxy-id="editingProxyId"
      v-model:selected-group-ids="editingGroupIds"
      :configuration-loading="configurationLoading"
      :configuration-ready="configurationReady"
      :account="editingAccount"
      :groups="groups"
      :groups-loading="groupsLoading"
      :saving="savingAccountEdit"
      @save="saveAccountEdit"
      @after-leave="clearCredentials"
    />

    <AccountBatchEditModal
      v-model="showBatchEditModal"
      v-model:enabled="batchSchedulingEnabled"
      v-model:concurrency-limit="batchConcurrencyLimit"
      v-model:weight="batchWeight"
      v-model:model-access="batchModelAccess"
      v-model:proxy-mode="batchProxyMode"
      v-model:proxy-id="batchProxyId"
      v-model:selected-group-ids="batchGroupIds"
      :catalog-account-id="batchCatalogAccountId"
      :selected-count="batchEditingCount"
      :groups="groups"
      :groups-loading="groupsLoading"
      :saving="savingBatchEdit"
      :has-changes="batchHasChanges"
      @save="saveBatchEdit"
    />

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="确认删除"
      description="删除后该账号将不再参与调度，此操作不可撤销"
      destructive
      confirm-text="确认删除"
      :loading="batchDeleting"
      @confirm="handleBatchDelete"
    >
      <p class="m-0">
        确定要删除选中的 {{ deleteCount }} 个账号吗？此操作不可撤销
      </p>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="showSingleDeleteModal"
      title="删除账号"
      description="删除后该账号将不再参与调度，此操作不可撤销"
      destructive
      confirm-text="确认删除"
      :loading="deletingAccount"
      @confirm="handleDelete"
    >
      <p class="m-0">
        确定要删除
        {{
          pendingDeleteAccount?.email
            || pendingDeleteAccount?.accountId
            || pendingDeleteAccount?.id
            || '该账号'
        }}
        吗？
      </p>
    </BaseConfirmModal>
  </div>
</template>
