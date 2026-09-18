<script setup lang="ts">
import type { PricingRow } from './model'
import type { ModelPricing, PricingChange } from '@/api'
import { CircleAlert } from '@lucide/vue'
import { computed, ref, shallowRef } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import PricingBatchModal from './PricingBatchModal.vue'
import PricingEditor from './PricingEditor.vue'
import PricingSyncModal from './PricingSyncModal.vue'
import PricingTable from './PricingTable.vue'
import PricingToolbar from './PricingToolbar.vue'
import { usePricing } from './usePricing'

const { catalog, provider, search, source, page, pageSize, selected, error, rows, visible, pagination, loading, saving, syncing, preview, load, save, startSync, confirmSync, toggle, togglePage } = usePricing()
const editorOpen = ref(false)
const editing = shallowRef<PricingRow>()
const batchOpen = ref(false)
const batchReset = ref(false)
const targets = shallowRef<PricingRow[]>([])
const disabled = computed(() => loading.value || saving.value || !!error.value)

function edit(row?: PricingRow) {
  editing.value = row
  editorOpen.value = true
}
function batch(reset: boolean) {
  targets.value = rows.value.filter(row => selected.value.includes(row.model))
  batchReset.value = reset
  batchOpen.value = true
}
async function saveModel(model: string, pricing: ModelPricing) {
  if (await save([model], { action: 'replace', pricing }))
    editorOpen.value = false
}
async function saveBatch(change: PricingChange) {
  if (await save(targets.value.map(row => row.model), change))
    batchOpen.value = false
}
</script>

<template>
  <section class="flex min-h-0 min-w-0 flex-col rounded-cp-card bg-cp-bg-container p-3 shadow-cp-card sm:p-4" aria-label="模型定价">
    <PricingToolbar
      v-model:search="search"
      v-model:provider="provider"
      v-model:source="source"
      :selected-count="selected.length"
      :disabled="disabled"
      :saving="saving"
      :syncing="syncing"
      :synced-at="catalog.syncedAt"
      @add="edit()"
      @sync="startSync"
      @set-multiplier="batch(false)"
      @reset="batch(true)"
      @clear="selected = []"
    />
    <PricingTable class="min-h-0 flex-1" :rows="error ? [] : visible" :selected="selected" :loading="loading" :disabled="disabled" @toggle="toggle" @toggle-page="togglePage" @edit="edit">
      <template v-if="error" #empty>
        <BaseEmpty title="价目加载失败" :description="error" :icon="CircleAlert" surface="none" class="w-full max-w-80" role="alert">
          <template #action>
            <BaseButton :loading="loading" @click="load">
              重试
            </BaseButton>
          </template>
        </BaseEmpty>
      </template>
    </PricingTable>
    <BaseTablePagination :pagination="pagination" :loading="loading" @page-change="page = $event" @page-size-change="pageSize = $event" />
    <PricingEditor v-model="editorOpen" :row="editing" :provider="provider" :saving="saving" @save="saveModel" />
    <PricingBatchModal v-model="batchOpen" :rows="targets" :reset="batchReset" :saving="saving" @confirm="saveBatch" />
    <PricingSyncModal :preview="preview" :catalog="catalog" :saving="saving" @close="preview = undefined" @confirm="confirmSync" />
  </section>
</template>
