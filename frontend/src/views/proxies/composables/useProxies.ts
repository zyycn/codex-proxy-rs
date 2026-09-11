import type { OutboundProxy, OutboundProxyTestResult } from '@/api'
import { watchDebounced } from '@vueuse/core'
import { computed, onMounted, ref, shallowRef, watch } from 'vue'

import {
  createOutboundProxy,
  deleteOutboundProxy,
  getOutboundProxies,
  testOutboundProxy,
  updateOutboundProxy,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useIdSet } from '@/composables/useIdSet'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { errorMessage } from '@/utils/async'
import { formatDateTime } from '@/utils/date'

export interface ProxyFormValue {
  name: string
  url: string
  targetUrl: string
}

function emptyForm(): ProxyFormValue {
  return { name: '', url: '', targetUrl: '' }
}

export function useProxies() {
  const searchQuery = shallowRef('')
  const showFormModal = shallowRef(false)
  const showDeleteModal = shallowRef(false)
  const editingProxy = shallowRef<OutboundProxy | null>(null)
  const pendingDeleteProxy = shallowRef<OutboundProxy | null>(null)
  const form = ref<ProxyFormValue>(emptyForm())
  const savingAction = useAsyncAction()
  const deletingAction = useAsyncAction()
  const formTestingAction = useAsyncAction()
  const formTestResult = shallowRef<OutboundProxyTestResult | null>(null)
  const testingProxies = useIdSet<string>()
  const testResults = ref<Map<string, OutboundProxyTestResult>>(new Map())

  const query = usePagedQuery({
    initialPageSize: 20,
    load: ({ page, pageSize }) =>
      getOutboundProxies({
        page,
        pageSize,
        search: searchQuery.value.trim() || undefined,
      }),
    onError: error => toast.error(errorMessage(error, '代理列表加载失败')),
  })

  const proxies = computed(() => query.items.value.map(proxy => ({
    ...proxy,
    updatedAtDisplay: formatDateTime(proxy.updatedAt),
  })))
  const pagination = computed(() => ({
    currentPage: query.page.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
  }))
  const saving = savingAction.loading
  const deleting = deletingAction.loading
  const formTesting = formTestingAction.loading
  const testingProxyIds = testingProxies.ids

  function openCreate() {
    editingProxy.value = null
    form.value = emptyForm()
    formTestResult.value = null
    showFormModal.value = true
  }

  function openEdit(proxy: OutboundProxy) {
    editingProxy.value = proxy
    form.value = { name: proxy.name, url: '', targetUrl: '' }
    formTestResult.value = null
    showFormModal.value = true
  }

  function validateForm() {
    const name = form.value.name.trim()
    if (!name) {
      toast.warning('请输入代理名称')
      return null
    }
    const url = form.value.url.trim()
    if (!editingProxy.value && !url) {
      toast.warning('请输入代理 URL')
      return null
    }
    return { name, url }
  }

  async function save() {
    if (saving.value)
      return
    const values = validateForm()
    if (!values)
      return

    await savingAction.run(async () => {
      const updating = Boolean(editingProxy.value)
      if (editingProxy.value) {
        await updateOutboundProxy({
          id: editingProxy.value.id,
          name: values.name,
          url: values.url || undefined,
        })
      }
      else {
        await createOutboundProxy({ name: values.name, url: values.url })
      }
      showFormModal.value = false
      editingProxy.value = null
      form.value = emptyForm()
      await query.execute()
      toast.success(updating ? '代理已更新' : '代理已添加')
    }, {
      errorText: editingProxy.value ? '代理更新失败' : '代理添加失败',
    })
  }

  /** 表单内测试：未保存时测 URL，编辑且未改 URL 时测已保存代理。 */
  async function testForm() {
    if (formTesting.value)
      return
    const url = form.value.url.trim()
    const id = editingProxy.value?.id
    if (!url && !id) {
      toast.warning('请输入代理 URL')
      return
    }
    const targetUrl = form.value.targetUrl.trim() || undefined
    formTestResult.value = null
    await formTestingAction.run(async () => {
      formTestResult.value = url
        ? await testOutboundProxy({ url, targetUrl })
        : await testOutboundProxy({ id, targetUrl })
    }, { errorText: '代理测试失败' })
  }

  async function testProxy(proxy: OutboundProxy) {
    await testingProxies.run(proxy.id, async () => {
      try {
        const result = await testOutboundProxy({ id: proxy.id })
        const results = new Map(testResults.value)
        results.set(proxy.id, result)
        testResults.value = results
        if (result.success)
          toast.success(`「${proxy.name}」连通正常（${result.latencyMs}ms）`)
        else
          toast.error(`「${proxy.name}」测试失败：${result.error ?? '未知错误'}`)
      }
      catch (error: unknown) {
        toast.error(errorMessage(error, '代理测试失败'))
      }
    })
  }

  function requestDelete(proxy: OutboundProxy) {
    pendingDeleteProxy.value = proxy
    showDeleteModal.value = true
  }

  async function confirmDelete() {
    const proxy = pendingDeleteProxy.value
    if (!proxy || deleting.value)
      return
    await deletingAction.run(async () => {
      await deleteOutboundProxy({ id: proxy.id })
      showDeleteModal.value = false
      pendingDeleteProxy.value = null
      await query.execute()
      toast.success('代理已删除')
    }, { errorText: '删除代理失败', onError: () => void query.execute() })
  }

  function handlePageChange(page: number) {
    query.page.value = page
    void query.execute()
  }

  function handlePageSizeChange(pageSize: number) {
    query.pageSize.value = pageSize
    query.page.value = 1
    void query.execute()
  }

  watchDebounced(searchQuery, () => {
    query.page.value = 1
    void query.execute()
  }, { debounce: 250 })
  watch(showFormModal, (open) => {
    if (!open && !saving.value) {
      editingProxy.value = null
      form.value = emptyForm()
      formTestResult.value = null
    }
  })

  onMounted(() => {
    void query.execute()
  })

  return {
    proxies,
    loading: query.loading,
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
  }
}
