<script setup lang="ts">
import type { OutboundProxyRecord } from '@/api'
import { LockKeyhole, Pencil, Plus, RefreshCw, Search, Trash2, Users, Wifi } from '@lucide/vue'
import { watchDebounced } from '@vueuse/core'
import { computed, onMounted, reactive, ref, shallowRef, watch } from 'vue'
import { createProxy, deleteProxy, getProxies, testProxy, updateProxy } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { errorMessage } from '@/utils/async'
import { formatDateTime } from '@/utils/date'
import ProxyAccountsModal from './components/ProxyAccountsModal.vue'
import ProxyFormModal from './components/ProxyFormModal.vue'

const search = shallowRef('')
const query = usePagedQuery({
  initialPageSize: 20,
  load: pagination => getProxies({ ...pagination, search: search.value.trim() || undefined }),
  onError: error => toast.error(errorMessage(error, '代理列表加载失败')),
})
const { items: proxies, loading } = query
const pagination = computed(() => ({ currentPage: query.page.value, pageSize: query.pageSize.value, total: query.total.value }))
const columns = defineTableColumns<OutboundProxyRecord>([
  { key: 'identity', label: '代理', kind: 'identity' },
  { key: 'exitIp', label: '出口 IP', kind: 'custom' },
  { key: 'latency', label: '耗时', kind: 'custom', size: 'sm' },
  { key: 'accounts', label: '关联账号', kind: 'custom', size: 'sm' },
  { key: 'testedAt', label: '测试时间', kind: 'datetime' },
  { key: 'actions', label: '操作', kind: 'actions' },
])
const showForm = shallowRef(false)
const editing = shallowRef<OutboundProxyRecord | null>(null)
const form = reactive({ name: '', proxyUrl: '' })
const saveAction = useAsyncAction()
const { loading: saving } = saveAction
const deleteAction = useAsyncAction()
const { loading: deleting } = deleteAction
const showDelete = shallowRef(false)
const pendingDelete = shallowRef<OutboundProxyRecord | null>(null)
const testingIds = ref(new Set<string>())
const showAccounts = shallowRef(false)
const inspected = shallowRef<OutboundProxyRecord | null>(null)

function openForm(proxy: OutboundProxyRecord | null = null) {
  editing.value = proxy
  form.name = proxy?.name ?? ''
  form.proxyUrl = ''
  showForm.value = true
}

async function checkProxy(proxy: OutboundProxyRecord) {
  if (testingIds.value.has(proxy.id))
    return
  testingIds.value.add(proxy.id)
  try {
    const result = await testProxy({ id: proxy.id, revision: proxy.revision })
    if (result.lastTest?.success)
      toast.success(`${result.name}：连接成功`)
    else
      toast.error(result.lastTest?.message ?? '代理测试失败')
  }
  catch (error) {
    toast.error(errorMessage(error, '代理测试失败'))
  }
  finally {
    testingIds.value.delete(proxy.id)
    await query.execute({ silent: true })
  }
}

async function save(testAfter: boolean) {
  if (saving.value)
    return
  const name = form.name.trim()
  const proxyUrl = form.proxyUrl.trim()
  if (!name || (!editing.value && !proxyUrl)) {
    toast.warning('请填写代理名称和连接地址')
    return
  }
  await saveAction.run(async () => {
    // 编辑时留空保留已保存的地址和认证，不能用脱敏地址覆盖原连接。
    const result = editing.value
      ? await updateProxy({ id: editing.value.id, revision: editing.value.revision, name, proxyUrl: proxyUrl || undefined })
      : await createProxy({ name, proxyUrl })
    showForm.value = false
    form.proxyUrl = ''
    toast.success('代理已保存')
    search.value = ''
    query.page.value = 1
    await query.execute()
    if (testAfter)
      await checkProxy(result.record)
  }, { errorText: '代理保存失败' })
}

function requestDelete(proxy: OutboundProxyRecord) {
  pendingDelete.value = proxy
  showDelete.value = true
}

async function confirmDelete() {
  const proxy = pendingDelete.value
  if (!proxy || deleting.value)
    return
  await deleteAction.run(async () => {
    await deleteProxy({ id: proxy.id, revision: proxy.revision })
    showDelete.value = false
    await query.execute()
    toast.success('代理已删除')
  }, { errorText: '代理删除失败，请确认没有账号使用该代理' })
}

function setPage(page: number) {
  query.page.value = page
  void query.execute()
}

function setPageSize(size: number) {
  query.pageSize.value = size
  setPage(1)
}

watch(showForm, (open) => {
  if (!open) {
    form.proxyUrl = ''
  }
})
watchDebounced(search, () => setPage(1), { debounce: 300 })
onMounted(() => void query.execute())
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
            <BaseIconButton label="刷新代理列表" :loading="loading" @click="query.execute()">
              <RefreshCw class="size-4" />
            </BaseIconButton>
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
          <BaseTable class="min-h-0 flex-1" :columns="columns" :rows="proxies" :loading="loading" :empty-text="search.trim() ? '没有找到匹配的代理，请尝试其他名称' : '暂无代理，请点击新增代理添加'">
            <template #identity="{ row }">
              <div class="grid min-w-0 gap-1">
                <strong class="truncate text-cp text-cp-text" :title="row.name">{{ row.name }}</strong>
                <span class="flex min-w-0 items-center gap-1 text-cp-xs font-emphasis text-cp-text-quaternary">
                  <LockKeyhole v-if="row.hasAuthentication" class="size-3 shrink-0" aria-label="已保存代理认证" />
                  <span class="truncate font-mono" :title="row.endpoint">{{ row.endpoint }}</span>
                </span>
              </div>
            </template>
            <template #exitIp="{ row }">
              <span class="break-all font-mono text-cp-xs">{{ row.lastTest?.exitIp ?? '-' }}</span>
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
              {{ row.lastTestAt ? formatDateTime(row.lastTestAt) : '-' }}
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
      :proxy="editing"
      :saving="saving"
      @save="save"
    />
    <BaseConfirmModal v-model="showDelete" title="删除代理" destructive :loading="deleting" @confirm="confirmDelete">
      <p class="m-0">
        确定删除“{{ pendingDelete?.name }}”吗？
      </p>
    </BaseConfirmModal>
    <ProxyAccountsModal v-model="showAccounts" :proxy="inspected" @removed="query.execute({ silent: true })" />
  </div>
</template>
