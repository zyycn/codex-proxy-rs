import type { OutboundProxyRecord, OutboundProxyTest } from '@/api'
import { toast } from '@codex-proxy/ui'
import { computed, onMounted, reactive, shallowRef, watch } from 'vue'
import { createProxy, deleteProxy, getProxies, probeProxy, testProxy, updateProxy } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useIdSet } from '@/composables/useIdSet'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { normalizeRequestLocation, requestLocationError } from '@/utils/location'

export function useProxies() {
  const search = shallowRef('')
  const query = usePagedQuery({
    initialPageSize: 20,
    load: (pagination, options) => getProxies({ ...pagination, search: search.value.trim() || undefined }, options),
  })
  const { items: proxies, loading } = query
  const pagination = computed(() => ({ currentPage: query.page.value, pageSize: query.pageSize.value, total: query.total.value }))
  const showForm = shallowRef(false)
  const editing = shallowRef<OutboundProxyRecord | null>(null)
  const form = reactive({
    name: '',
    proxyUrl: '',
    customLocation: false,
    location: { country: '', region: '', city: '', timezone: '' },
  })
  const formTestResult = shallowRef<OutboundProxyTest | null>(null)
  const saveAction = useAsyncAction()
  const { loading: saving } = saveAction
  const deleteAction = useAsyncAction()
  const { loading: deleting } = deleteAction
  const showDelete = shallowRef(false)
  const pendingDelete = shallowRef<OutboundProxyRecord | null>(null)
  const testing = useIdSet<string>()
  const testingIds = testing.ids
  const formTestAction = useAsyncAction()
  const testingForm = computed(() => formTestAction.loading.value || (editing.value !== null && testingIds.value.has(editing.value.id)))
  const detectingLocation = shallowRef(false)
  const testingConnection = computed(() => testingForm.value && !detectingLocation.value)

  function openForm(proxy: OutboundProxyRecord | null = null) {
    editing.value = proxy
    form.name = proxy?.name ?? ''
    form.proxyUrl = ''
    formTestResult.value = null
    const currentLocation = proxy?.autoLocation ? proxy.detectedLocation?.location ?? proxy.location : proxy?.location
    form.customLocation = currentLocation != null
    form.location = currentLocation ? { ...currentLocation } : { country: '', region: '', city: '', timezone: '' }
    showForm.value = true
  }

  async function checkProxy(proxy: OutboundProxyRecord) {
    if (testing.has(proxy.id))
      return
    testing.add(proxy.id)
    try {
      const result = await testProxy({ id: proxy.id, revision: proxy.revision })
      if (editing.value?.id === result.id) {
        editing.value = result
        formTestResult.value = result.lastTest
      }
      if (result.lastTest?.success === false)
        toast.error(`${result.name}：${result.lastTest.message}`)
      else if (result.lastTest?.success)
        toast.success(`${result.name}：连接成功`)
      else
        toast.error(result.lastTest?.message ?? '代理测试失败')
    }
    catch {}
    finally {
      testing.remove(proxy.id)
      await query.execute({ silent: true })
    }
  }

  async function testConnection() {
    if (saving.value || testingForm.value)
      return
    const proxyUrl = form.proxyUrl.trim()
    if (!proxyUrl && editing.value) {
      await checkProxy(editing.value)
      return
    }
    if (!proxyUrl) {
      toast.warning('请填写代理连接地址')
      return
    }
    await formTestAction.run(async () => {
      // 新地址只做探测，保存前不修改代理及关联账号的连接配置。
      const result = await probeProxy({ proxyUrl, detectLocation: false })
      formTestResult.value = result
      if (!result.success)
        toast.error(result.message)
      else
        toast.success(`连接成功，耗时 ${result.latencyMs} ms`)
    })
  }

  async function detectLocation() {
    if (saving.value || testingForm.value)
      return
    const proxyUrl = form.proxyUrl.trim()
    if (!proxyUrl && !editing.value) {
      toast.warning('请填写代理连接地址')
      return
    }
    detectingLocation.value = true
    try {
      await formTestAction.run(async () => {
        let result: OutboundProxyTest | null
        if (proxyUrl) {
          // 新地址只解析草稿，不修改已保存代理或账号绑定。
          result = await probeProxy({ proxyUrl, detectLocation: true })
        }
        else {
          const proxy = editing.value!
          const updated = await testProxy({ id: proxy.id, revision: proxy.revision, detectLocation: true })
          editing.value = updated
          result = updated.lastTest
          await query.execute({ silent: true })
        }
        formTestResult.value = result
        if (!result?.success) {
          toast.error(result?.message ?? '代理连接失败，未能解析位置')
          return
        }
        if (result.location.status === 'detected') {
          form.location = { ...result.location.location }
          form.customLocation = true
          toast.success('已填入出口位置')
        }
        else if (result.location.status === 'conflict') {
          toast.warning('IPv4 与 IPv6 出口时区不一致，请手动填写')
        }
        else if (result.location.status === 'failed') {
          toast.warning(result.location.message)
        }
        else {
          toast.warning('未获取到出口位置')
        }
      })
    }
    finally {
      detectingLocation.value = false
    }
  }

  async function save() {
    if (saving.value || testingForm.value)
      return
    const name = form.name.trim()
    const proxyUrl = form.proxyUrl.trim()
    if (!name || (!editing.value && !proxyUrl)) {
      toast.warning('请填写代理名称和连接地址')
      return
    }
    const location = form.customLocation ? normalizeRequestLocation(form.location) : null
    const locationError = location ? requestLocationError(location) : ''
    if (locationError) {
      toast.warning(locationError)
      return
    }
    await saveAction.run(async () => {
      // 编辑时留空保留已保存的地址和认证，不能用脱敏地址覆盖原连接。
      await (editing.value
        ? updateProxy({
            id: editing.value.id,
            revision: editing.value.revision,
            name,
            autoLocation: false,
            proxyUrl: proxyUrl || undefined,
            location,
          })
        : createProxy({
            name,
            autoLocation: false,
            proxyUrl,
            location,
          }))
      showForm.value = false
      toast.success('代理已保存')
      search.value = ''
      query.page.value = 1
      await query.execute()
    })
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
    })
  }

  function setPage(page: number) {
    query.page.value = page
    void query.execute()
  }

  function setPageSize(size: number) {
    query.pageSize.value = size
    setPage(1)
  }

  watch(() => form.proxyUrl, () => {
    formTestResult.value = null
  })

  function clearCredentials() {
    form.proxyUrl = ''
  }

  watch(search, (_value, _previous, onCleanup) => {
    const timer = setTimeout(() => {
      void setPage(1)
    }, 300)
    onCleanup(() => clearTimeout(timer))
  })
  onMounted(() => void query.execute())

  return {
    search,
    proxies,
    loading,
    pagination,
    loadProxies: query.execute,
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
  }
}
