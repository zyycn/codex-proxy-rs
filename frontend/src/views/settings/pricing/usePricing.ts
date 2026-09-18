import type { PricingCatalog, PricingChange, PricingSyncPreview } from '@/api'
import { computed, onMounted, ref, shallowRef, watch } from 'vue'
import { getPricing, previewPricingSync, syncPricing, updatePricing } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { errorMessage } from '@/utils/async'
import { pricingRows } from './model'

export function usePricing() {
  const catalog = shallowRef<PricingCatalog>({ defaults: {}, overrides: {}, synced: {}, syncedAt: null })
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
      error.value = ''
      page.value = Math.min(page.value, Math.max(1, Math.ceil(filtered.value.length / pageSize.value)))
    }, { onError: (cause) => { error.value = errorMessage(cause, '无法加载价目') } })
  }
  async function save(models: string[], change: PricingChange): Promise<boolean> {
    return await writeAction.run(async () => {
      await updatePricing({ provider: provider.value, models, change })
      toast.success(change.action === 'reset' ? '已清除人工覆盖' : '模型定价已保存')
      selected.value = []
      await load()
      return true
    }) ?? false
  }
  async function startSync() {
    await previewAction.run(async () => {
      preview.value = await previewPricingSync()
    })
  }
  async function confirmSync() {
    if (!preview.value)
      return
    const approved = preview.value
    await writeAction.run(async () => {
      await syncPricing(approved)
      preview.value = undefined
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
  return { catalog, provider, search, source, page, pageSize, selected, error, rows, filtered, visible, pagination, loading: loadAction.loading, saving: writeAction.loading, syncing: previewAction.loading, preview, load, save, startSync, confirmSync, toggle, togglePage }
}
