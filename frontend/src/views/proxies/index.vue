<script setup lang="ts">
import type { OutboundProxyRecord } from '@/api'
import { BaseButton, BaseCard, BaseConfirmModal, BaseIconButton, BaseInput, BasePageHeader, BaseTable, BaseTablePagination } from '@codex-proxy/ui'
import { LockKeyhole, MapPin, Pencil, Plus, Search, Trash2, Users, Wifi } from '@lucide/vue'
import { shallowRef } from 'vue'
import ProxyAccountsModal from './components/ProxyAccountsModal.vue'
import ProxyFormModal from './components/ProxyFormModal.vue'
import { useProxies } from './composables/useProxies'
import { proxyColumns } from './constants'
import { effectiveProxyLocation } from './utils/location'

const {
  search,
  proxies,
  loading,
  pagination,
  loadProxies,
  showForm,
  editing,
  form,
  formTestResult,
  saving,
  showDelete,
  pendingDelete,
  deleting,
  testingIds,
  testingConnection,
  detectingLocation,
  openForm,
  checkProxy,
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
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <BasePageHeader
      class="h-17"
      title="代理管理"
      description="管理账号使用的代理，测试连接并查看出口 IP"
    />
    <BaseCard class="mt-5 flex h-[calc(100dvh-136px)] min-h-125 flex-col">
      <template #header>
        <div class="flex w-full flex-col gap-3 sm:flex-row sm:items-center">
          <BaseInput v-model="search" class="sm:w-80" aria-label="搜索代理" placeholder="搜索代理名称...">
            <template #prefix>
              <Search class="size-4.5 text-cp-text-tertiary" />
            </template>
          </BaseInput>
          <div class="flex shrink-0 items-center justify-end gap-2 sm:ml-auto">
            <BaseButton variant="primary" @click="openForm()">
              <template #icon>
                <Plus class="size-4" />
              </template>
              新增代理
            </BaseButton>
          </div>
        </div>
      </template>
      <template #body>
        <div class="flex h-full min-h-0 flex-col">
          <BaseTable class="min-h-0 flex-1" :columns="proxyColumns" :rows="proxies" :loading="loading" :empty-text="search.trim() ? '没有找到匹配的代理，请尝试其他名称' : '暂无代理，请点击新增代理添加'">
            <template #name="{ row }">
              <span class="block truncate text-cp text-cp-text" :title="row.name">{{ row.name }}</span>
            </template>
            <template #address="{ row }">
              <div class="grid min-w-0 gap-1">
                <span class="flex min-w-0 items-center gap-1 text-cp-xs font-emphasis text-cp-text-quaternary">
                  <LockKeyhole v-if="row.hasAuthentication" class="size-3 shrink-0" aria-label="已保存代理认证" />
                  <span class="truncate font-mono" :title="row.endpoint">{{ row.endpoint }}</span>
                </span>
                <span v-if="effectiveProxyLocation(row)" class="flex min-w-0 items-center gap-1 text-cp-xs text-cp-text-secondary">
                  <MapPin class="size-3 shrink-0" aria-hidden="true" />
                  <span class="truncate" :title="`${effectiveProxyLocation(row)?.city} · ${effectiveProxyLocation(row)?.timezone}`">{{ effectiveProxyLocation(row)?.city }} · {{ effectiveProxyLocation(row)?.timezone }}</span>
                </span>
              </div>
            </template>
            <template #exitIp="{ row }">
              <div v-if="row.lastTest?.exitIpv4 && row.lastTest?.exitIpv6" class="flex flex-col gap-0.5 font-mono text-cp-xs">
                <span class="truncate" :title="`IPv4: ${row.lastTest.exitIpv4}`">
                  {{ row.lastTest.exitIpv4 }}
                </span>
                <span class="truncate" :title="`IPv6: ${row.lastTest.exitIpv6}`">
                  {{ row.lastTest.exitIpv6 }}
                </span>
              </div>
              <div v-else-if="row.lastTest?.exitIpv4" class="truncate font-mono text-cp-xs" :title="`IPv4: ${row.lastTest.exitIpv4}`">
                {{ row.lastTest.exitIpv4 }}
              </div>
              <div v-else-if="row.lastTest?.exitIpv6" class="truncate font-mono text-cp-xs" :title="`IPv6: ${row.lastTest.exitIpv6}`">
                {{ row.lastTest.exitIpv6 }}
              </div>
              <span v-else-if="row.lastTest?.exitIp" class="block truncate font-mono text-cp-xs" :title="row.lastTest.exitIp">{{ row.lastTest.exitIp }}</span>
              <span v-else class="text-cp-text-quaternary">-</span>
            </template>
            <template #latency="{ row }">
              <span v-if="testingIds.has(row.id)" class="text-cp-text-secondary">测试中</span>
              <span v-else-if="row.lastTest?.success" class="tabular-nums text-cp-success">
                {{ row.lastTest.latencyMs }} ms
              </span>
              <span v-else-if="row.lastTest" class="text-cp-error" :title="`${row.lastTest.message}（耗时 ${row.lastTest.latencyMs} ms）`">失败</span>
              <span v-else class="text-cp-text-quaternary">未测试</span>
            </template>
            <template #accounts="{ row }">
              <button type="button" class="inline-flex cursor-pointer items-center gap-1.5 rounded-sm border-0 bg-transparent p-0 text-cp-sm text-cp-text-secondary outline-none transition-colors hover:text-cp-primary-text focus-visible:ring-2 focus-visible:ring-cp-control-outline focus-visible:ring-offset-2 focus-visible:ring-offset-cp-bg-container" :aria-label="`查看 ${row.name} 的 ${row.accountCount} 个关联账号`" @click="inspected = row; showAccounts = true">
                <Users class="size-3.5" aria-hidden="true" />
                <span class="font-mono tabular-nums">{{ row.accountCount }}</span>
              </button>
            </template>
            <template #testedAt="{ row }">
              {{ row.lastTestAtDisplay ?? '-' }}
            </template>
            <template #actions="{ row }">
              <div class="flex items-center gap-1">
                <BaseIconButton size="sm" label="测试代理" :loading="testingIds.has(row.id)" :disabled="testingIds.has(row.id)" @click="checkProxy(row)">
                  <Wifi class="size-3.5 text-cp-link" />
                </BaseIconButton>
                <BaseIconButton size="sm" label="编辑代理" :disabled="testingIds.has(row.id)" @click="openForm(row)">
                  <Pencil class="size-3.5 text-cp-link" />
                </BaseIconButton>
                <BaseIconButton size="sm" :label="row.accountCount ? '代理正在被账号使用' : '删除代理'" :disabled="row.accountCount > 0 || testingIds.has(row.id)" @click="requestDelete(row)">
                  <Trash2 class="size-3.5 text-cp-error" />
                </BaseIconButton>
              </div>
            </template>
          </BaseTable>
          <BaseTablePagination :pagination="pagination" :loading="loading" @page-change="setPage" @page-size-change="setPageSize" />
        </div>
      </template>
    </BaseCard>

    <ProxyFormModal
      v-model="showForm"
      v-model:name="form.name"
      v-model:proxy-url="form.proxyUrl"
      v-model:custom-location="form.customLocation"
      v-model:location="form.location"
      :test-result="formTestResult"
      :proxy="editing"
      :saving="saving"
      :testing-connection="testingConnection"
      :detecting-location="detectingLocation"
      @save="save"
      @test="testConnection"
      @detect-location="detectLocation"
      @after-leave="clearCredentials"
    />
    <BaseConfirmModal v-model="showDelete" title="删除代理" destructive :loading="deleting" @confirm="confirmDelete">
      <p class="m-0">
        确定删除“{{ pendingDelete?.name }}”吗？
      </p>
    </BaseConfirmModal>
    <ProxyAccountsModal v-model="showAccounts" :proxy="inspected" @removed="loadProxies({ silent: true })" />
  </div>
</template>
