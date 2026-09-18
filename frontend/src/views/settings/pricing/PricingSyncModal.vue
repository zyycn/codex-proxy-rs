<script setup lang="ts">
import type { PricingCatalog, PricingSyncPreview } from '@/api'
import { computed } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseTag from '@/components/base/BaseTag.vue'
import { bands, effectivePrice, priceFields } from './model'

const props = defineProps<{ preview?: PricingSyncPreview, catalog: PricingCatalog, saving: boolean }>()
defineEmits<{ close: [], confirm: [] }>()
const changes = computed(() => {
  const items: { id: string, custom: boolean, details: { label: string, before: string, after: string }[] }[] = []
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
      items.push({ id: `${provider}/${model}`, details, custom: !!props.catalog.overrides[provider]?.[model] })
    }
  }
  return items
})
</script>

<template>
  <BaseModal :model-value="!!preview" title="确认同步来源价目" description="models.dev · USD / 1M Tokens" size="lg" :dismissible="!saving" @update:model-value="!$event && $emit('close')">
    <div class="grid gap-4">
      <p class="m-0 text-cp text-cp-text-secondary">
        {{ changes.length }} 个模型的来源价格将变化。人工单价和倍率保留；未覆盖的档位会采用新的来源价格。models.dev 是社区价目，不代表订阅实际扣费。
      </p>
      <div class="max-h-80 overflow-auto rounded-cp bg-cp-fill-quaternary p-4">
        <div v-for="item in changes" :key="item.id" class="grid gap-2 py-3 text-cp-sm">
          <div class="min-w-0">
            <div class="break-all font-mono">
              {{ item.id }}
            </div><span v-if="item.custom" class="text-cp-xs text-cp-primary-text">保留人工覆盖</span>
          </div>
          <div v-for="detail in item.details" :key="detail.label" class="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 text-cp-xs">
            <span class="text-cp-text-secondary">{{ detail.label }}</span>
            <span class="flex items-center gap-2 font-mono tabular-nums">
              <span class="text-cp-text-tertiary">{{ detail.before }}</span><span aria-hidden="true">→</span><span class="text-cp-primary-text">{{ detail.after }}</span>
            </span>
          </div>
        </div>
        <p v-if="!changes.length" class="m-0 text-cp-sm text-cp-text-secondary">
          来源价格没有变化。
        </p>
      </div>
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
      <BaseButton :disabled="saving" @click="$emit('close')">
        取消
      </BaseButton><BaseButton variant="primary" :loading="saving" @click="$emit('confirm')">
        确认同步
      </BaseButton>
    </template>
  </BaseModal>
</template>
