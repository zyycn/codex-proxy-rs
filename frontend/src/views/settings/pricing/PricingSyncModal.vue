<script setup lang="ts">
import type { PricingCatalog, PricingSyncPreview } from '@/api'
import { BaseButton, BaseCheckbox, BaseModal, BaseScrollbar, BaseTag } from '@codex-proxy/ui'
import { computed, shallowRef, watch } from 'vue'
import { bands, effectivePrice, priceFields } from './model'

const props = defineProps<{ preview?: PricingSyncPreview, catalog: PricingCatalog, saving: boolean }>()
const emit = defineEmits<{ confirm: [models: Record<string, string[]>] }>()
const open = defineModel<boolean>({ required: true })
const selected = shallowRef(new Set<string>())
const changes = computed(() => {
  const items: { id: string, provider: string, model: string, custom: boolean, details: { label: string, before: string, after: string }[] }[] = []
  const providers = new Set([...Object.keys(props.catalog.synced), ...Object.keys(props.preview?.prices ?? {})])
  for (const provider of providers) {
    const previous = props.catalog.synced[provider] ?? {}
    const next = props.preview?.prices[provider] ?? {}
    for (const model of new Set([...Object.keys(previous), ...Object.keys(next)])) {
      if (JSON.stringify(previous[model]) === JSON.stringify(next[model]))
        continue
      const details = []
      for (const band of bands) {
        const before = previous[model]?.bands[band.value] ?? props.catalog.defaults[provider]?.[model]?.bands[band.value]
        const after = next[model]?.bands[band.value] ?? props.catalog.defaults[provider]?.[model]?.bands[band.value]
        for (const field of priceFields) {
          if (before?.[field.key] !== after?.[field.key]) {
            details.push({
              label: `${band.label} · ${field.label}`,
              before: effectivePrice(before?.[field.key]),
              after: effectivePrice(after?.[field.key]),
            })
          }
        }
      }
      items.push({ id: `${provider}/${model}`, provider, model, details, custom: !!props.catalog.overrides[provider]?.[model] })
    }
  }
  return items
})
const selectedChanges = computed(() => changes.value.filter(item => selected.value.has(item.id)))
const allSelected = computed(() => changes.value.length > 0 && selectedChanges.value.length === changes.value.length)
const partiallySelected = computed(() => selectedChanges.value.length > 0 && !allSelected.value)

watch(() => props.preview, () => {
  selected.value = new Set(props.preview ? changes.value.map(item => item.id) : [])
}, { immediate: true })

function toggle(id: string, checked: boolean) {
  const next = new Set(selected.value)
  if (checked)
    next.add(id)
  else
    next.delete(id)
  selected.value = next
}

function confirm() {
  if (props.saving || !selectedChanges.value.length)
    return
  const models: Record<string, string[]> = {}
  for (const item of selectedChanges.value)
    (models[item.provider] ??= []).push(item.model)
  emit('confirm', models)
}
</script>

<template>
  <BaseModal v-model="open" title="确认同步来源价目" description="USD / 1M Tokens" size="lg" :dismissible="!saving">
    <div class="grid gap-4">
      <p class="m-0 text-cp text-cp-text-secondary">
        仅同步所选模型的来源价格，保留自定义单价和倍率
      </p>
      <div v-if="changes.length" class="flex items-center justify-between gap-3">
        <BaseCheckbox
          :model-value="allSelected"
          :indeterminate="partiallySelected"
          label="全选"
          show-label
          :disabled="saving"
          @update:model-value="selected = new Set($event ? changes.map(item => item.id) : [])"
        />
        <span class="text-cp-sm text-cp-text-tertiary" aria-live="polite">已选 {{ selectedChanges.length }} / {{ changes.length }}</span>
      </div>
      <BaseScrollbar max-height="20rem" class="rounded-cp bg-cp-fill-quaternary">
        <div class="p-4">
          <div v-for="item in changes" :key="item.id" class="grid gap-2 py-3 text-cp-sm">
            <BaseCheckbox
              :model-value="selected.has(item.id)"
              :label="item.id"
              show-label
              :disabled="saving"
              @update:model-value="toggle(item.id, $event)"
            >
              <template #label>
                <span class="break-all font-mono text-cp-sm font-normal">{{ item.id }}</span>
              </template>
            </BaseCheckbox>
            <span v-if="item.custom" class="pl-6.5 text-cp-xs text-cp-primary-text">保留人工覆盖</span>
            <div v-if="item.details.length" class="ml-6.5 grid gap-1">
              <div v-for="detail in item.details" :key="detail.label" class="grid grid-cols-2 items-center gap-x-4 rounded-cp-sm bg-cp-bg-container px-2 py-1.5 text-cp-xs">
                <span class="text-cp-text-secondary">{{ detail.label }}</span>
                <span class="flex items-center gap-2 font-mono tabular-nums">
                  <span class="text-cp-text-tertiary">{{ detail.before }}</span><span aria-hidden="true">→</span><span class="text-cp-primary-text">{{ detail.after }}</span>
                </span>
              </div>
            </div>
          </div>
          <p v-if="!changes.length" class="m-0 text-cp-sm text-cp-text-secondary">
            来源价格没有变化
          </p>
        </div>
      </BaseScrollbar>
      <details v-if="preview?.skipped.length" class="text-cp-sm text-cp-text-secondary">
        <summary class="cursor-pointer">
          跳过 {{ preview.skipped.length }} 个缺少完整价格或计价方式不匹配的模型
        </summary>
        <BaseScrollbar max-height="10rem" class="mt-3">
          <ul class="m-0 flex list-none flex-wrap gap-2 p-0 pr-2" aria-label="跳过同步的模型">
            <li v-for="model in preview.skipped" :key="model" class="flex max-w-full min-w-0">
              <BaseTag class="font-mono">
                {{ model }}
              </BaseTag>
            </li>
          </ul>
        </BaseScrollbar>
      </details>
    </div>
    <template #footer>
      <BaseButton :disabled="saving" @click="open = false">
        取消
      </BaseButton><BaseButton variant="primary" :loading="saving" :disabled="!selectedChanges.length" @click="confirm">
        确认同步
      </BaseButton>
    </template>
  </BaseModal>
</template>
