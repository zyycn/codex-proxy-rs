<script setup lang="ts">
import { ChevronDown } from '@lucide/vue'
import { ref } from 'vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import AccountConnectionTestModal from './components/AccountConnectionTestModal.vue'
import AccountCreateModal from './components/AccountCreateModal.vue'
import AccountFilters from './components/AccountFilters.vue'
import AccountIdentityCell from './components/AccountIdentityCell.vue'
import AccountOverviewCards from './components/AccountOverviewCards.vue'
import AccountPlanBadge from './components/AccountPlanBadge.vue'
import AccountQuotaPanel from './components/AccountQuotaPanel.vue'
import AccountQuotaSummaryCell from './components/AccountQuotaSummaryCell.vue'
import AccountStatusBadge from './components/AccountStatusBadge.vue'
import AccountTableActions from './components/AccountTableActions.vue'
import AccountUsagePanel from './components/AccountUsagePanel.vue'
import { useAccountConnectionTest } from './composables/useAccountConnectionTest'
import { useAccountMutations } from './composables/useAccountMutations'
import { useAccountsQuery } from './composables/useAccountsQuery'
import { useAccountsTable } from './composables/useAccountsTable'
import { accountColumns, derivedAccountStatus } from './constants'

const selectedIds = ref<Set<string>>(new Set())
const {
  loading,
  accounts,
  loadAccounts,
  searchQuery,
  providerQuery,
  statusQuery,
  sort,
  accountSummary,
  accountPagination,
  replaceAccount,
  handlePageChange,
  handlePageSizeChange,
  handleSortChange,
} = useAccountsQuery()

const {
  showCreateModal,
  showDeleteModal,
  showSingleDeleteModal,
  pendingDeleteAccount,
  refreshingAccountIds,
  refreshingQuotaAccountIds,
  updatingStatusAccountIds,
  deletingAccount,
  creatingAccount,
  authorizingOAuth,
  batchDeleting,
  exportingAccounts,
  reauthorizingAccount,
  createForm,
  handleCreate,
  handleAuthorizeOAuth,
  openCreateAccount,
  openReauthorizeAccount,
  requestDeleteAccount,
  handleDelete,
  handleBatchDelete,
  handleExportAccounts,
  handleRefresh,
  handleRefreshQuota,
  handleToggleSchedule,
  scheduleActionLabel,
} = useAccountMutations({
  accounts,
  selectedIds,
  reload: loadAccounts,
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
} = useAccountConnectionTest({ reload: loadAccounts })

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
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <BasePageHeader
      class="h-17"
      title="账号管理"
      description="维护账号池，查看可用性、配额与使用状态"
    />

    <AccountOverviewCards :summary="accountSummary" />

    <BaseCard
      class="mt-4 flex h-[calc(100dvh-250px)] min-h-125 flex-col"
      body-class="mt-3 flex min-h-0 flex-1"
    >
      <template #header>
        <AccountFilters
          v-model:search="searchQuery"
          v-model:status="statusQuery"
          v-model:provider="providerQuery"
          :selected-count="selectedIds.size"
          :batch-deleting="batchDeleting"
          :exporting-accounts="exportingAccounts"
          @delete-selected="showDeleteModal = true"
          @export-selected="handleExportAccounts"
          @create="openCreateAccount"
        />
      </template>

      <template #body>
        <BaseTable
          class="min-h-0 flex-1"
          :columns="accountColumns"
          :rows="accounts"
          :loading="loading"
          :selected-row-keys="selectedRowKeys"
          :expanded-row-keys="expandedRowKeys"
          :pagination="accountPagination"
          :sort="sort"
          empty-text="暂无账号数据"
          min-width="1480px"
          @page-change="handlePageChange"
          @page-size-change="handlePageSizeChange"
          @sort-change="handleSortChange"
        >
          <template #expander="{ row }">
            <button
              type="button"
              class="inline-flex size-6 cursor-pointer items-center justify-center rounded-md border-0 bg-transparent text-(--cp-text-secondary) transition hover:bg-(--cp-default-bg-hover) hover:text-(--cp-text-primary)"
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
            <AccountIdentityCell :account="row" />
          </template>

          <template #provider="{ row }">
            <ProviderIconGroup
              :provider="row.provider"
              :authentication-kind="row.authenticationKind"
            />
          </template>

          <template #status="{ row }">
            <AccountStatusBadge
              :status="derivedAccountStatus(row)"
              :error-reason="row.errorReason"
              :error-message="row.errorMessage"
              :rate-limited-until="row.quota.rateLimitedUntil"
            />
          </template>

          <template #planType="{ row }">
            <AccountPlanBadge :plan-type="row.planType" />
          </template>

          <template #usage="{ row }">
            <AccountQuotaSummaryCell :account="row" />
          </template>

          <template #actions="{ row }">
            <AccountTableActions
              :account="row"
              :deleting="deletingAccount"
              :refreshing="refreshingAccountIds.has(row.id)"
              :schedule-label="scheduleActionLabel(row)"
              :testing="testingConnectionIds.has(row.id)"
              :updating-status="updatingStatusAccountIds.has(row.id)"
              @delete="requestDeleteAccount"
              @refresh="handleRefresh"
              @reauthorize="openReauthorizeAccount"
              @test="openConnectionTest"
              @toggle-schedule="handleToggleSchedule"
            />
          </template>

          <template #expanded="{ row }">
            <div class="grid gap-3 p-4 lg:grid-cols-[1.05fr_2.45fr]">
              <AccountQuotaPanel
                :account="row"
                :refreshing="refreshingQuotaAccountIds.has(row.id)"
                @refresh-quota="handleRefreshQuota"
              />
              <AccountUsagePanel :account="row" />
            </div>
          </template>
        </BaseTable>
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

    <AccountCreateModal
      v-model="showCreateModal"
      v-model:form="createForm"
      :account="reauthorizingAccount"
      :oauth-loading="authorizingOAuth"
      :reauthorizing="Boolean(reauthorizingAccount)"
      :saving="creatingAccount"
      @create="handleCreate"
      @generate-oauth="handleAuthorizeOAuth"
    />

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="确认删除"
      description="删除后该账号将不再参与调度，此操作不可撤销"
      variant="danger"
      confirm-text="确认删除"
      :loading="batchDeleting"
      width="480px"
      @confirm="handleBatchDelete"
    >
      <p class="m-0">
        确定要删除选中的 {{ selectedIds.size }} 个账号吗？此操作不可撤销
      </p>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="showSingleDeleteModal"
      title="删除账号"
      description="删除后该账号将不再参与调度，此操作不可撤销"
      variant="danger"
      confirm-text="确认删除"
      :loading="deletingAccount"
      width="480px"
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
