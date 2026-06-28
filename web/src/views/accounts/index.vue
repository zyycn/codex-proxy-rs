<script setup lang="ts">
import { ref } from 'vue'
import { ChevronDown, Download, Plus, Search, Trash2 } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { accountColumns } from './constants'
import { useAccountConnectionTest } from './composables/useAccountConnectionTest'
import { useAccountFilters } from './composables/useAccountFilters'
import { useAccountMutations } from './composables/useAccountMutations'
import { useAccountsTable } from './composables/useAccountsTable'
import AccountConnectionTestModal from './components/AccountConnectionTestModal.vue'
import AccountCreateModal from './components/AccountCreateModal.vue'
import AccountEditModal from './components/AccountEditModal.vue'
import AccountIdentityCell from './components/AccountIdentityCell.vue'
import AccountOverviewCards from './components/AccountOverviewCards.vue'
import AccountQuotaSummaryCell from './components/AccountQuotaSummaryCell.vue'
import AccountQuotaPanel from './components/AccountQuotaPanel.vue'
import AccountStatusBadge from './components/AccountStatusBadge.vue'
import AccountTableActions from './components/AccountTableActions.vue'
import AccountUsagePanel from './components/AccountUsagePanel.vue'

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
  connectionTestSelectedModel,
  connectionTestModelOptions,
  connectionTestStatusView,
  openConnectionTest,
  handleTestConnection,
} = useAccountConnectionTest()

const selectedIds = ref<Set<string>>(new Set())
const totalAccounts = ref(0)

const {
  page,
  pageSize,
  searchQuery,
  accountPagination,
  bindAccountLoader,
  handlePageChange,
  handlePageSizeChange,
} = useAccountFilters(totalAccounts)

const {
  loading,
  accounts,
  accountSummary,
  showCreateModal,
  showDeleteModal,
  showSingleDeleteModal,
  showEditModal,
  editingAccount,
  pendingDeleteAccount,
  refreshingAccountIds,
  refreshingQuotaAccountIds,
  updatingStatusAccountIds,
  deletingAccount,
  creatingAccount,
  authorizingOAuth,
  savingAccount,
  batchDeleting,
  exportingAccounts,
  createForm,
  editForm,
  editStatusModel,
  loadAccounts,
  handleCreate,
  handleAuthorizeOAuth,
  openEditAccount,
  handleSaveAccount,
  requestDeleteAccount,
  handleDelete,
  handleBatchDelete,
  handleExportAccounts,
  handleRefresh,
  handleRefreshQuota,
  handleToggleSchedule,
  scheduleActionLabel,
} = useAccountMutations({
  page,
  pageSize,
  searchQuery,
  selectedIds,
  totalAccounts,
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

bindAccountLoader(loadAccounts)

function planTypeLabel(planType?: string) {
  return planType?.trim() || 'Free'
}

function planTypeClass(planType?: string) {
  const normalized = planTypeLabel(planType).toLowerCase()
  if (normalized.includes('enterprise') || normalized.includes('team')) {
    return 'bg-(--cp-bg-dark) text-(--cp-white)'
  }
  if (normalized.includes('pro')) {
    return 'bg-(--cp-info-bg) text-(--cp-info-text)'
  }
  if (normalized.includes('plus') || normalized.includes('basic')) {
    return 'bg-(--cp-normal-bg) text-(--cp-normal-text)'
  }
  return 'bg-(--cp-bg-subtle) text-(--cp-text-secondary)'
}
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <header class="flex h-17 shrink-0 items-start justify-between">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          账号管理
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          维护 Codex 账号池，快速确认可用性、配额与连接状态。
        </p>
      </div>
    </header>

    <AccountOverviewCards :summary="accountSummary" />

    <BaseCard
      :padded="false"
      class="mt-4 flex h-[calc(100vh-248px)] min-h-125 flex-col"
      header-class="px-4 pt-4 pb-2 md:px-5"
      body-class="flex min-h-0 flex-1 px-4 pb-3 md:px-5"
    >
      <template #header>
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div class="flex min-w-0 flex-1 flex-wrap items-center gap-3">
            <BaseInput
              v-model="searchQuery"
              placeholder="搜索邮箱或 ID..."
              class="w-80 max-w-full [--cp-input-current-bg:var(--cp-input-soft-bg)] [--cp-input-current-bg-hover:var(--cp-input-soft-bg-hover)]"
            >
              <template #prefix>
                <Search class="size-4.5 text-(--cp-text-tertiary)" />
              </template>
            </BaseInput>

            <BaseButton
              v-if="selectedIds.size > 0"
              variant="danger"
              :disabled="batchDeleting"
              @click="showDeleteModal = true"
            >
              <Trash2 class="size-4" />
              删除选中 ({{ selectedIds.size }})
            </BaseButton>
          </div>

          <div class="flex shrink-0 items-center gap-2">
            <BaseButton
              variant="default"
              :loading="exportingAccounts"
              :disabled="accounts.length === 0"
              @click="handleExportAccounts"
            >
              <Download class="size-4" />
              {{ selectedIds.size > 0 ? '导出选中' : '导出' }}
            </BaseButton>
            <BaseButton variant="primary" @click="showCreateModal = true">
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
          empty-text="暂无账号数据"
          @page-change="handlePageChange"
          @page-size-change="handlePageSizeChange"
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

          <template #status="{ row }">
            <AccountStatusBadge :status="row.status" />
          </template>

          <template #planType="{ row }">
            <span
              class="inline-flex items-center rounded-full px-2.5 py-1 text-[12px] font-[760] capitalize"
              :class="planTypeClass(row.planType)"
            >
              {{ planTypeLabel(row.planType) }}
            </span>
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
              @edit="openEditAccount"
              @refresh="handleRefresh"
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
      :logs="connectionTestLogs"
      :model="connectionTestModel"
      :model-options="connectionTestModelOptions"
      :started-at="connectionTestStartedAt"
      :status="connectionTestStatus"
      :status-view="connectionTestStatusView"
      @test="handleTestConnection()"
    />

    <AccountCreateModal
      v-model="showCreateModal"
      v-model:form="createForm"
      :oauth-loading="authorizingOAuth"
      :saving="creatingAccount"
      @create="handleCreate"
      @generate-oauth="handleAuthorizeOAuth"
    />

    <AccountEditModal
      v-model="showEditModal"
      v-model:form="editForm"
      v-model:status="editStatusModel"
      :account="editingAccount"
      :saving="savingAccount"
      @save="handleSaveAccount"
    />

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="确认删除"
      description="删除后该账号将不再参与调度，此操作不可撤销。"
      variant="danger"
      confirm-text="确认删除"
      :loading="batchDeleting"
      width="480px"
      @confirm="handleBatchDelete"
    >
      <p class="m-0">确定要删除选中的 {{ selectedIds.size }} 个账号吗？此操作不可撤销。</p>
    </BaseConfirmModal>

    <BaseConfirmModal
      v-model="showSingleDeleteModal"
      title="删除账号"
      description="删除后该账号将不再参与调度，此操作不可撤销。"
      variant="danger"
      confirm-text="确认删除"
      :loading="deletingAccount"
      width="480px"
      @confirm="handleDelete"
    >
      <p class="m-0">
        确定要删除
        {{
          pendingDeleteAccount?.email ||
          pendingDeleteAccount?.accountId ||
          pendingDeleteAccount?.id ||
          '该账号'
        }}
        吗？
      </p>
    </BaseConfirmModal>
  </div>
</template>
