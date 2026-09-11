<script setup lang="ts">
import BaseCard from '@/components/base/BaseCard.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import BaseTable from '@/components/base/BaseTable/index.vue'
import ProxyActions from './components/ProxyActions.vue'
import ProxyFilters from './components/ProxyFilters.vue'
import ProxyFormModal from './components/ProxyFormModal.vue'
import { useProxies } from './composables/useProxies'
import { proxyColumns } from './constants'

const {
  proxies,
  loading,
  pagination,
  searchQuery,
  showFormModal,
  showDeleteModal,
  editingProxy,
  pendingDeleteProxy,
  form,
  saving,
  deleting,
  formTesting,
  formTestResult,
  testingProxyIds,
  testResults,
  openCreate,
  openEdit,
  save,
  testForm,
  testProxy,
  requestDelete,
  confirmDelete,
  handlePageChange,
  handlePageSizeChange,
} = useProxies()
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <BasePageHeader
      class="h-17"
      title="IP 管理"
      description="维护出站代理 IP 池，并为每个账号指定独立的出站隧道"
    />

    <BaseCard
      class="mt-5 flex h-[calc(100dvh-136px)] min-h-125 flex-col"
    >
      <template #header>
        <ProxyFilters
          v-model:search="searchQuery"
          @create="openCreate"
        />
      </template>

      <template #body>
        <div class="flex h-full min-h-0 flex-col">
          <BaseTable
            class="min-h-0 flex-1"
            :columns="proxyColumns"
            :rows="proxies"
            :loading="loading"
            empty-text="暂无代理，请点击添加代理创建"
          >
            <template #identity="{ row }">
              <div class="grid min-w-0 gap-1">
                <strong class="truncate text-cp text-cp-text">
                  {{ row.name }}
                </strong>
              </div>
            </template>

            <template #endpoint="{ row }">
              <span class="font-mono text-cp-xs break-all text-cp-text-secondary">
                {{ row.endpoint }}
              </span>
            </template>

            <template #accountCount="{ row }">
              <span
                class="inline-flex h-6 items-center rounded-lg px-2 text-cp-xs font-bold"
                :class="row.accountCount > 0
                  ? 'bg-cp-info-container text-cp-info-on-container'
                  : 'bg-cp-fill-tertiary text-cp-text-quaternary'"
              >
                {{ row.accountCount }} 个账号
              </span>
            </template>

            <template #test="{ row }">
              <span
                v-if="testResults.get(row.id)"
                class="inline-flex h-6 items-center rounded-lg px-2 text-cp-xs font-bold"
                :class="testResults.get(row.id)!.success
                  ? 'bg-cp-success-container text-cp-success-on-container'
                  : 'bg-cp-error-container text-cp-error-on-container'"
              >
                {{ testResults.get(row.id)!.success
                  ? `可用 ${testResults.get(row.id)!.latencyMs}ms`
                  : '不可用' }}
              </span>
              <span v-else class="text-cp-xs text-cp-text-quaternary">未测试</span>
            </template>

            <template #actions="{ row }">
              <ProxyActions
                :proxy="row"
                :testing="testingProxyIds.has(row.id)"
                :deleting="deleting"
                @edit="openEdit"
                @test="testProxy"
                @delete="requestDelete"
              />
            </template>
          </BaseTable>
          <BaseTablePagination
            :pagination="pagination"
            :loading="loading"
            @page-change="handlePageChange"
            @page-size-change="handlePageSizeChange"
          />
        </div>
      </template>
    </BaseCard>

    <ProxyFormModal
      v-model="showFormModal"
      v-model:form="form"
      :proxy="editingProxy"
      :saving="saving"
      :testing="formTesting"
      :test-result="formTestResult"
      @save="save"
      @test="testForm"
    />

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="删除代理"
      description="删除后该代理将从 IP 池移除；已把它设为出站隧道的账号会保留原地址继续生效。"
      destructive
      confirm-text="确认删除"
      :loading="deleting"
      @confirm="confirmDelete"
    >
      <p class="m-0">
        确定删除“{{ pendingDeleteProxy?.name || '该代理' }}”吗？
      </p>
      <p v-if="pendingDeleteProxy?.accountCount" class="mt-2 mb-0 text-cp-warning-text">
        当前有 {{ pendingDeleteProxy.accountCount }} 个账号在使用该代理地址。
      </p>
    </BaseConfirmModal>
  </div>
</template>
