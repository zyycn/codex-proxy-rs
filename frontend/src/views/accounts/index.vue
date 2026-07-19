<script setup lang="ts">
import { ChevronDown, Download, Plus, Search, Trash2 } from '@lucide/vue'
import { ref } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import ProviderBadge from '@/components/ProviderBadge.vue'
import AccountConnectionTestModal from './components/AccountConnectionTestModal.vue'
import AccountCreateModal from './components/AccountCreateModal.vue'
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
import { accountColumns, accountStatusFilterOptions } from './constants'

const selectedIds = ref<Set<string>>(new Set())
const providerFilterOptions = [
  { label: '全部平台', value: '' },
  { label: 'OpenAI', value: 'openai' },
  { label: 'xAI', value: 'xai' },
]

const {
  totalAccounts,
  loading,
  accounts,
  loadAccounts,
  searchQuery,
  providerQuery,
  statusQuery,
  sort,
  accountSummary,
  configRevision,
  accountPagination,
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
  loadingProviderInstances,
  batchDeleting,
  exportingAccounts,
  reauthorizingAccount,
  providerInstanceOptions,
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
  patchAccountStatus,
  handleToggleSchedule,
  scheduleActionLabel,
} = useAccountMutations({
  accounts,
  accountSummary,
  statusQuery,
  sort,
  selectedIds,
  totalAccounts,
  configRevision,
  reload: loadAccounts,
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
} = useAccountConnectionTest({
  onAccountStatus: patchAccountStatus,
})

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
      :padded="false"
      class="mt-4 flex h-[calc(100dvh-248px)] min-h-125 flex-col"
      header-class="px-4 pt-4 pb-2 md:px-5"
      body-class="flex min-h-0 flex-1 px-4 pb-3 md:px-5"
    >
      <template #header>
        <div
          class="flex w-full flex-col gap-3 md:flex-row md:flex-wrap md:items-center"
          role="group"
          aria-label="账号筛选与操作"
        >
          <div class="flex min-w-0 items-center gap-2 md:flex-none md:gap-3">
            <BaseInput
              v-model="searchQuery"
              placeholder="搜索邮箱或 ID..."
              class="min-w-0 flex-1 [--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] md:w-80 md:flex-none"
            >
              <template #prefix>
                <Search class="size-4.5 text-(--cp-text-tertiary)" />
              </template>
            </BaseInput>

            <BaseSelect
              v-model="providerQuery"
              :options="providerFilterOptions"
              aria-label="按平台筛选"
              class="w-34 shrink-0 [--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] md:w-40"
            />

            <BaseSelect
              v-model="statusQuery"
              :options="accountStatusFilterOptions"
              aria-label="按账号状态筛选"
              class="w-34 shrink-0 [--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)] md:w-40"
            />
          </div>

          <div class="flex shrink-0 self-end items-center justify-end gap-2 md:ml-auto">
            <BaseButton
              v-if="selectedIds.size > 0"
              variant="danger"
              :disabled="batchDeleting"
              @click="showDeleteModal = true"
            >
              <Trash2 class="size-4" />
              删除选中 ({{ selectedIds.size }})
            </BaseButton>
            <BaseButton
              v-if="selectedIds.size > 0"
              variant="default"
              :loading="exportingAccounts"
              @click="handleExportAccounts"
            >
              <Download class="size-4" />
              导出选中 ({{ selectedIds.size }})
            </BaseButton>
            <BaseButton variant="primary" @click="openCreateAccount">
              <Plus class="size-4" />
              添加账号
            </BaseButton>
          </div>
        </div>
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
            <ProviderBadge :provider="row.provider" />
          </template>

          <template #status="{ row }">
            <AccountStatusBadge :status="row.tokenRefreshing ? 'refreshing' : row.status" />
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
              :refreshing="refreshingAccountIds.has(row.id) || row.tokenRefreshing"
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
      :loading-provider-instances="loadingProviderInstances"
      :oauth-loading="authorizingOAuth"
      :provider-instance-options="providerInstanceOptions"
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
