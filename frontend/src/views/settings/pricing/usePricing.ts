import type { PricingCatalog, PricingChange, PricingSyncPreview } from '@/api'
import { toast } from '@codex-proxy/ui'
import { computed, onMounted, ref, shallowRef, watch } from 'vue'
import { getPricing, previewPricingSync, syncPricing, updatePricing } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { errorMessage } from '@/utils/operation'
import { pricingProviders, pricingRows } from './model'

export function usePricing() {
  const catalog = shallowRef<PricingCatalog>({ defaults: {}, overrides: {}, synced: {}, syncedAt: null, syncedAtDisplay: null })
  const provider = ref('openai')
  const search = ref('')
  const source = ref('all')
  const page = ref(1)
  const pageSize = ref(20)
  const selected = ref<string[]>([])
  const error = ref('')
  const loadAction = useAsyncAction()
  const writeAction = useAsyncAction()
  const previewAction = useAsyncAction()
  const preview = shallowRef<PricingSyncPreview>()
  const syncOpen = shallowRef(false)
  // 确认窗口展示查询时的价格对比，保存后的列表刷新不能改变退场中的预览。
  const syncCatalog = shallowRef(catalog.value)
  const providers = computed(() => pricingProviders(catalog.value))
  const rows = computed(() => pricingRows(catalog.value, provider.value))
  const filtered = computed(() => rows.value.filter(row => row.model.toLowerCase().includes(search.value.trim().toLowerCase())
    && (source.value === 'all' || row.source === source.value)))
  const visible = computed(() => filtered.value.slice((page.value - 1) * pageSize.value, page.value * pageSize.value))
  const pagination = computed(() => ({ currentPage: page.value, pageSize: pageSize.value, total: filtered.value.length }))
  watch([provider, search, source, pageSize], () => {
    page.value = 1
  })
  // 筛选变更清空选择，避免批量修改屏幕之外的隐藏目标。
  watch([provider, search, source], () => {
    selected.value = []
  })

  async function load() {
    await loadAction.run(async () => {
      catalog.value = await getPricing()
      if (!providers.value.includes(provider.value))
        provider.value = providers.value[0] ?? ''
      error.value = ''
      page.value = Math.min(page.value, Math.max(1, Math.ceil(filtered.value.length / pageSize.value)))
    }, { onError: (cause) => { error.value = errorMessage(cause, '无法加载价目') } })
  }
  async function save(models: string[], change: PricingChange): Promise<boolean> {
    return await writeAction.run(async () => {
      await updatePricing({ provider: provider.value, models, change })
      toast.success(change.action === 'delete' ? '模型价目已删除' : change.action === 'reset' ? '已清除人工覆盖' : '模型定价已保存')
      selected.value = []
      await load()
      return true
    }) ?? false
  }
  async function startSync() {
    await previewAction.run(async () => {
      preview.value = await previewPricingSync()
      syncCatalog.value = catalog.value
      syncOpen.value = true
    })
  }
  async function confirmSync(models: Record<string, string[]>) {
    if (!syncOpen.value || !preview.value || !Object.values(models).some(items => items.length))
      return
    const approved = preview.value
    await writeAction.run(async () => {
      await syncPricing({ preview: approved, models })
      syncOpen.value = false
      toast.success('来源价目已同步，人工覆盖保持不变')
      await load()
    })
  }
  function toggle(model: string, checked: boolean) {
    selected.value = checked ? [...new Set([...selected.value, model])] : selected.value.filter(id => id !== model)
  }
  function togglePage(checked: boolean) {
    const ids = visible.value.map(row => row.model)
    selected.value = checked ? [...new Set([...selected.value, ...ids])] : selected.value.filter(id => !ids.includes(id))
  }
  onMounted(load)
  return { catalog, providers, provider, search, source, page, pageSize, selected, error, rows, filtered, visible, pagination, loading: loadAction.loading, saving: writeAction.loading, syncing: previewAction.loading, preview, syncOpen, syncCatalog, load, save, startSync, confirmSync, toggle, togglePage }
}
