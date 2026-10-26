import type { OutboundProxyRecord, OutboundProxyTest } from '@/api'
import type { RequestOptions } from '@/api/request'
import { computed, onMounted, reactive, shallowRef, watch } from 'vue'
import { createProxy, deleteProxy, getProxies, probeProxy, testProxy, updateProxy } from '@/api'
import { ApiError } from '@/api/request'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useIdSet } from '@/composables/useIdSet'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { t } from '@/i18n'
import { normalizeRequestLocation, requestLocationError } from '@/utils/location'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage } from '@/utils/operation'
import { proxyResultData } from '../utils/batch'

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
    maxWebsocketConnections: '',
    customLocation: false,
    location: {
      country: '',
      region: '',
      city: '',
      timezone: '',
    },
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
  const updating = useIdSet<string>()
  const updatingIds = updating.ids
  const showDisable = shallowRef(false)
  const pendingDisable = shallowRef<OutboundProxyRecord | null>(null)
  const disabling = computed(() => pendingDisable.value !== null && updating.has(pendingDisable.value.id))
  const formTestAction = useAsyncAction()
  const testingForm = computed(() => formTestAction.loading.value || (editing.value !== null && testingIds.value.has(editing.value.id)))
  const detectingLocation = shallowRef(false)
  const testingConnection = computed(() => testingForm.value && !detectingLocation.value)

  function openForm(proxy: OutboundProxyRecord | null = null) {
    editing.value = proxy
    form.name = proxy?.name ?? ''
    form.proxyUrl = ''
    form.maxWebsocketConnections = proxy?.maxWebsocketConnections
      ? String(proxy.maxWebsocketConnections)
      : ''
    formTestResult.value = null
    const currentLocation = proxy?.autoLocation
      ? proxy.detectedLocation?.location ?? proxy.location
      : proxy?.location
    form.customLocation = currentLocation != null
    form.location = currentLocation
      ? { ...currentLocation }
      : {
          country: '',
          region: '',
          city: '',
          timezone: '',
        }
    showForm.value = true
  }

  async function testSavedProxies(targets: OutboundProxyRecord[], options: RequestOptions = {}, detectLocation = false) {
    if (targets.some(proxy => testing.has(proxy.id)))
      throw new ApiError(t('proxies.actions.testInProgress'), 429)
    for (const proxy of targets)
      testing.add(proxy.id)
    try {
      const results = await testProxy(targets.map(proxy => ({ id: proxy.id, revision: proxy.revision, detectLocation })), options)
      for (const [index, result] of results.entries()) {
        const target = targets[index]
        if (result.status !== 'succeeded' || !target)
          continue
        const record = result.data
        // 列表或编辑目标已换版本时，迟到的测试不能覆盖新配置
        proxies.value = proxies.value.map(item => item.id === record.id && item.revision === target.revision
          ? record
          : item)
        if (editing.value?.id === record.id && editing.value.revision === target.revision) {
          editing.value = record
          formTestResult.value = record.lastTest
        }
      }
      return results
    }
    finally {
      for (const proxy of targets)
        testing.remove(proxy.id)
    }
  }

  async function checkProxy(proxy: OutboundProxyRecord) {
    if (testing.has(proxy.id))
      return
    try {
      const result = proxyResultData((await testSavedProxies([proxy], {}, true))[0])
      const test = result.lastTest
      if (!test?.success) {
        ZNotification.error({ message: `${result.name}：${test?.message ?? t('proxies.actions.testFailed')}` })
      }
      else if (test.location.status === 'failed') {
        ZNotification.warning({
          message: t('proxies.actions.connectedWithDetails', { name: result.name, message: test.location.message }),
        })
      }
      else if (test.location.status === 'conflict') {
        ZNotification.warning({
          message: t('proxies.actions.timeZoneMismatch', { name: result.name }),
        })
      }
      else {
        ZNotification.success({
          message: t('proxies.actions.connected', { name: result.name }),
        })
      }
    }
    catch (error) {
      if (!(error instanceof ApiError)) {
        ZNotification.error({
          message: errorMessage(error, t('proxies.actions.testFailed')),
        })
      }
    }
    finally {
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
      ZNotification.warning({
        message: t('proxies.actions.fillInProxyConnectionAddress'),
      })
      return
    }
    await formTestAction.run(async () => {
      // 新地址只做探测，保存前不修改代理及关联账号的连接配置。
      const result = await probeProxy({ proxyUrl, detectLocation: false })
      formTestResult.value = result
      if (!result.success) {
        ZNotification.error({ message: result.message })
      }
      else {
        ZNotification.success({
          message: t('proxies.actions.connectedInMs', { latencyMs: result.latencyMs }),
        })
      }
    })
  }

  async function detectLocation() {
    if (saving.value || testingForm.value)
      return
    const proxyUrl = form.proxyUrl.trim()
    if (!proxyUrl && !editing.value) {
      ZNotification.warning({
        message: t('proxies.actions.fillInProxyConnectionAddress'),
      })
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
          const updated = proxyResultData((await testProxy([
            { id: proxy.id, revision: proxy.revision, detectLocation: true },
          ]))[0])
          editing.value = updated
          result = updated.lastTest
          await query.execute({ silent: true })
        }
        formTestResult.value = result
        if (!result?.success) {
          ZNotification.error({ message: result?.message ?? t('proxies.actions.locationFailed') })
          return
        }
        if (result.location.status === 'detected') {
          form.location = { ...result.location.location }
          form.customLocation = true
          ZNotification.success({
            message: t('proxies.actions.exitLocationFilledIn'),
          })
        }
        else if (result.location.status === 'conflict') {
          ZNotification.warning({
            message: t('proxies.editor.timeZoneMismatch'),
          })
        }
        else if (result.location.status === 'failed') {
          ZNotification.warning({ message: result.location.message })
        }
        else {
          ZNotification.warning({
            message: t('proxies.actions.unableToObtainExitLocation'),
          })
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
      ZNotification.warning({
        message: t('proxies.actions.fieldsRequired'),
      })
      return
    }
    const maxWebsocketConnections = Number(form.maxWebsocketConnections)
    if (!Number.isInteger(maxWebsocketConnections) || maxWebsocketConnections < 0 || maxWebsocketConnections > 4294967295) {
      ZNotification.warning({
        message: t('proxies.actions.invalidConnectionLimit'),
      })
      return
    }
    const location = form.customLocation
      ? normalizeRequestLocation(form.location)
      : null
    const locationError = location
      ? requestLocationError(location)
      : ''
    if (locationError) {
      ZNotification.warning({ message: locationError })
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
            maxWebsocketConnections,
            proxyUrl: proxyUrl || undefined,
            location,
          })
        : createProxy([
            {
              name,
              autoLocation: false,
              maxWebsocketConnections,
              proxyUrl,
              location,
            },
          ]).then(results => proxyResultData(results[0])))
      showForm.value = false
      ZNotification.success({
        message: t('proxies.actions.saved'),
      })
      await reloadAfterCreate()
    })
  }

  async function reloadAfterCreate() {
    search.value = ''
    await query.reloadFromStart()
  }

  function requestDelete(proxy: OutboundProxyRecord) {
    pendingDelete.value = proxy
    showDelete.value = true
  }

  async function setEnabled(proxy: OutboundProxyRecord, enabled: boolean) {
    if (updating.has(proxy.id) || testing.has(proxy.id))
      return
    updating.add(proxy.id)
    try {
      await updateProxy({
        id: proxy.id,
        revision: proxy.revision,
        name: proxy.name,
        enabled,
      })
      showDisable.value = false
      ZNotification.success({
        message: enabled
          ? t('proxies.actions.proxyEnabled')
          : t('proxies.actions.disabled'),
      })
    }
    catch {}
    finally {
      updating.remove(proxy.id)
      await query.execute({ silent: true })
    }
  }

  function toggleEnabled(proxy: OutboundProxyRecord) {
    if (proxy.enabled) {
      pendingDisable.value = proxy
      showDisable.value = true
    }
    else {
      void setEnabled(proxy, true)
    }
  }

  async function confirmDisable() {
    if (pendingDisable.value)
      await setEnabled(pendingDisable.value, false)
  }

  async function confirmDelete() {
    const proxy = pendingDelete.value
    if (!proxy || deleting.value)
      return
    await deleteAction.run(async () => {
      await deleteProxy({ id: proxy.id, revision: proxy.revision })
      showDelete.value = false
      await query.execute()
      ZNotification.success({
        message: t('proxies.actions.deleted'),
      })
    })
  }

  function setPage(page: number) {
    void query.execute({ page })
  }

  function setPageSize(size: number) {
    void query.execute({ page: 1, pageSize: size })
  }

  watch(() => form.proxyUrl, () => {
    formTestResult.value = null
  })

  function clearCredentials() {
    form.proxyUrl = ''
  }

  watch(search, (_value, _previous, onCleanup) => {
    const timer = setTimeout(() => {
      void query.reloadFromStart()
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
    reloadAfterCreate,
    showForm,
    editing,
    form,
    formTestResult,
    saving,
    showDelete,
    pendingDelete,
    deleting,
    testingIds,
    updatingIds,
    showDisable,
    pendingDisable,
    disabling,
    toggleEnabled,
    confirmDisable,
    testingConnection,
    detectingLocation,
    openForm,
    checkProxy,
    testSavedProxies,
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
