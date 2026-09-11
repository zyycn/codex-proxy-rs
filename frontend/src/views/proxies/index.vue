<script setup lang="ts">
import type { OutboundProxyRecord } from '@/api'
import { Eye, EyeOff, LockKeyhole, Pencil, Plus, RefreshCw, Save, Search, Trash2, Users, Wifi } from '@lucide/vue'
import { watchDebounced } from '@vueuse/core'
import { computed, onMounted, reactive, ref, shallowRef, watch } from 'vue'
import { RouterLink } from 'vue-router'
import { createProxy, deleteProxy, getProxies, testProxy, updateProxy } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { errorMessage } from '@/utils/async'
import { formatDateTime } from '@/utils/date'

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
  { key: 'status', label: '测试状态', kind: 'custom', size: 'lg' },
  { key: 'exitIp', label: '出口 IP', kind: 'custom' },
  { key: 'latency', label: '耗时', kind: 'custom', size: 'sm' },
  { key: 'accounts', label: '关联账号', kind: 'custom', size: 'sm' },
  { key: 'testedAt', label: '测试时间', kind: 'datetime' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'lg' },
])
const showForm = shallowRef(false)
const editing = shallowRef<OutboundProxyRecord | null>(null)
const form = reactive({ name: '', proxyUrl: '' })
const replaceConnection = ref(true)
const showSecret = ref(false)
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
  replaceConnection.value = !proxy
  showSecret.value = false
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
  if (!name || (replaceConnection.value && !proxyUrl)) {
    toast.warning('请填写代理名称和连接地址')
    return
  }
  await saveAction.run(async () => {
    const result = editing.value
      ? await updateProxy({ id: editing.value.id, revision: editing.value.revision, name, proxyUrl: replaceConnection.value ? proxyUrl : undefined })
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
    showSecret.value = false
  }
})
watchDebounced(search, () => setPage(1), { debounce: 300 })
onMounted(() => void query.execute())
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <BasePageHeader title="代理管理" />
    <section class="mt-5 flex min-h-125 flex-1 flex-col bg-cp-bg-container p-5">
      <div class="mb-4 flex flex-wrap items-center justify-between gap-3">
        <BaseInput v-model="search" class="w-full sm:max-w-80" aria-label="搜索代理" placeholder="搜索代理名称">
          <template #prefix>
            <Search class="size-4" />
          </template>
        </BaseInput>
        <div class="ml-auto flex items-center gap-2">
          <BaseIconButton label="刷新代理列表" :disabled="loading" @click="query.execute()">
            <RefreshCw class="size-4" />
          </BaseIconButton>
          <BaseButton variant="primary" @click="openForm()">
            <Plus class="size-4" />新增代理
          </BaseButton>
        </div>
      </div>
      <BaseTable class="min-h-0 flex-1" :columns="columns" :rows="proxies" :loading="loading" empty-text="暂无代理">
        <template #identity="{ row }">
          <div class="grid min-w-0 gap-1">
            <strong class="truncate text-cp text-cp-text" :title="row.name">{{ row.name }}</strong>
            <span class="flex min-w-0 items-center gap-1 text-cp-xs text-cp-text-secondary">
              <LockKeyhole v-if="row.hasAuthentication" class="size-3 shrink-0" aria-label="已保存代理认证" />
              <span class="truncate font-mono" :title="row.endpoint">{{ row.endpoint }}</span>
            </span>
          </div>
        </template>
        <template #status="{ row }">
          <div class="grid min-w-0 gap-1">
            <span class="text-cp-sm font-semibold" :class="row.lastTest?.success ? 'text-cp-success' : row.lastTest ? 'text-cp-error' : 'text-cp-text-quaternary'">
              {{ testingIds.has(row.id) ? '测试中' : row.lastTest?.success ? '通过' : row.lastTest ? '失败' : '未测试' }}
            </span>
            <span v-if="row.lastTest && !row.lastTest.success" class="truncate text-cp-xs text-cp-text-secondary" :title="row.lastTest.message">{{ row.lastTest.message }}</span>
          </div>
        </template>
        <template #exitIp="{ row }">
          <span class="break-all font-mono text-cp-xs">{{ row.lastTest?.exitIp ?? '-' }}</span>
        </template>
        <template #latency="{ row }">
          {{ row.lastTest ? `${row.lastTest.latencyMs} ms` : '-' }}
        </template>
        <template #accounts="{ row }">
          <BaseButton variant="ghost" :aria-label="`查看 ${row.name} 的关联账号`" @click="inspected = row; showAccounts = true">
            <Users class="size-4" />{{ row.accounts.length }}
          </BaseButton>
        </template>
        <template #testedAt="{ row }">
          {{ row.lastTestAt ? formatDateTime(row.lastTestAt) : '-' }}
        </template>
        <template #actions="{ row }">
          <div class="flex items-center gap-1">
            <BaseIconButton label="测试代理" :loading="testingIds.has(row.id)" :disabled="testingIds.has(row.id)" @click="checkProxy(row)">
              <Wifi class="size-4 text-cp-link" />
            </BaseIconButton>
            <BaseIconButton label="编辑代理" :disabled="testingIds.has(row.id)" @click="openForm(row)">
              <Pencil class="size-4 text-cp-link" />
            </BaseIconButton>
            <BaseIconButton :label="row.accounts.length ? '代理正在被账号使用' : '删除代理'" :disabled="row.accounts.length > 0 || testingIds.has(row.id)" @click="requestDelete(row)">
              <Trash2 class="size-4 text-cp-error" />
            </BaseIconButton>
          </div>
        </template>
      </BaseTable>
      <BaseTablePagination :pagination="pagination" :loading="loading" @page-change="setPage" @page-size-change="setPageSize" />
    </section>

    <BaseModal v-model="showForm" :title="editing ? '编辑代理' : '新增代理'" size="md" :dismissible="!saving">
      <div class="grid gap-5">
        <BaseFormItem label="代理名称" required>
          <BaseInput v-model="form.name" maxlength="100" :disabled="saving" aria-label="代理名称" />
        </BaseFormItem>
        <template v-if="editing">
          <p class="m-0 break-all font-mono text-cp-sm text-cp-text-secondary">
            {{ editing.endpoint }}
          </p>
          <div class="flex items-center justify-between gap-3">
            <span class="text-cp text-cp-text-secondary">更换连接配置</span>
            <BaseSwitch v-model="replaceConnection" label="更换连接配置" :disabled="saving" />
          </div>
        </template>
        <BaseFormItem v-if="replaceConnection" label="代理 URL" required>
          <BaseInput v-model="form.proxyUrl" :type="showSecret ? 'text' : 'password'" autocomplete="new-password" :disabled="saving" aria-label="代理 URL" placeholder="socks5h://user:password@host:1080">
            <template #suffix>
              <BaseIconButton :label="showSecret ? '隐藏代理地址' : '显示代理地址'" @click="showSecret = !showSecret">
                <EyeOff v-if="showSecret" class="size-4" /><Eye v-else class="size-4" />
              </BaseIconButton>
            </template>
          </BaseInput>
        </BaseFormItem>
        <p v-if="editing?.accounts.length && replaceConnection" class="m-0 text-cp-sm text-cp-warning-text">
          将更新 {{ editing.accounts.length }} 个关联账号的出口。
        </p>
      </div>
      <template #footer>
        <BaseButton variant="ghost" :disabled="saving" @click="showForm = false">
          取消
        </BaseButton>
        <BaseButton variant="secondary" :loading="saving" @click="save(false)">
          <Save class="size-4" />保存代理
        </BaseButton>
        <BaseButton variant="primary" :loading="saving" @click="save(true)">
          <Wifi class="size-4" />保存并测试
        </BaseButton>
      </template>
    </BaseModal>
    <BaseConfirmModal v-model="showDelete" title="删除代理" destructive :loading="deleting" @confirm="confirmDelete">
      <p class="m-0">
        确定删除“{{ pendingDelete?.name }}”吗？
      </p>
    </BaseConfirmModal>
    <BaseModal v-model="showAccounts" title="关联账号" size="md">
      <ul v-if="inspected?.accounts.length" class="m-0 grid list-none gap-3 p-0">
        <li v-for="account in inspected.accounts" :key="account.id" class="break-all text-cp">
          {{ account.name }}
        </li>
      </ul>
      <p v-else class="m-0 text-cp-text-secondary">
        暂无关联账号
      </p>
      <template #footer>
        <RouterLink to="/accounts" class="text-cp-link">
          账号管理
        </RouterLink>
      </template>
    </BaseModal>
  </div>
</template>
