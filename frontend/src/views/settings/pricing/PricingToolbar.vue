<script setup lang="ts">
import { Openai, Xai } from '@boxicons/vue'
import { CircleAlert, Download, Percent, Plus, RotateCcw, Search, X } from '@lucide/vue'
import { computed, shallowRef, useId } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BasePopover from '@/components/base/BasePopover.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { PROVIDER_DISPLAY_NAMES } from '@/utils/providers'
import { sourceLabels } from './model'

const props = defineProps<{
  selectedCount: number
  disabled: boolean
  saving: boolean
  syncing: boolean
  syncedAt?: string | null
}>()
defineEmits<{ add: [], sync: [], setMultiplier: [], reset: [], clear: [] }>()
const search = defineModel<string>('search', { required: true })
const provider = defineModel<string>('provider', { required: true })
const source = defineModel<string>('source', { required: true })
const syncInfoOpen = shallowRef(false)
const syncInfoId = useId()
const syncTime = computed(() => props.syncedAt ? new Date(props.syncedAt).toLocaleString() : '尚未同步')
const providerOptions = [
  { label: PROVIDER_DISPLAY_NAMES.openai, value: 'openai', icon: Openai },
  { label: PROVIDER_DISPLAY_NAMES.xai, value: 'xai', icon: Xai },
]
</script>

<template>
  <div class="mb-3 shrink-0">
    <div class="flex flex-wrap items-center gap-2">
      <BaseInput v-model="search" class="w-full sm:w-72" placeholder="搜索模型…" aria-label="搜索模型 ID">
        <template #prefix>
          <Search class="size-4" />
        </template>
      </BaseInput>
      <BaseSegmented v-model="provider" class="w-21 shrink-0 bg-(--cp-input-bg)!" label="模型提供商" :options="providerOptions" :disabled="saving" display="icon" />
      <BaseSelect v-model="source" class="w-32" aria-label="价格来源" :options="[{ label: '全部来源', value: 'all' }, { label: sourceLabels.custom, value: 'custom' }, { label: sourceLabels.synced, value: 'synced' }, { label: sourceLabels.builtin, value: 'builtin' }]" />
      <div v-if="selectedCount" class="ml-auto flex w-full flex-wrap items-center justify-end gap-2 sm:w-auto" role="group" aria-label="批量操作">
        <span class="inline-flex h-cp-control w-full items-center gap-1 whitespace-nowrap text-cp text-cp-text-secondary sm:mr-1 sm:w-auto" aria-live="polite">
          已选 <span class="font-mono tabular-nums">{{ selectedCount }}</span> 项
        </span>
        <BaseButton :disabled="disabled || selectedCount > 500" @click="$emit('setMultiplier')">
          <template #icon>
            <Percent class="size-4" />
          </template>
          设置倍率
        </BaseButton>
        <BaseButton :disabled="disabled || selectedCount > 500" @click="$emit('reset')">
          <template #icon>
            <RotateCcw class="size-4" />
          </template>
          清除覆盖
        </BaseButton>
        <BaseIconButton label="取消选择" variant="filled" :disabled="disabled" @click="$emit('clear')">
          <X class="size-4" />
        </BaseIconButton>
      </div>
      <div v-else class="ml-auto flex items-center gap-2">
        <div class="flex items-center gap-1">
          <BasePopover v-model="syncInfoOpen" placement="bottom-end" trigger="hover-click">
            <template #trigger>
              <BaseIconButton label="同步说明" size="sm" :aria-expanded="syncInfoOpen" :aria-controls="syncInfoOpen ? syncInfoId : undefined">
                <CircleAlert class="size-3.5" />
              </BaseIconButton>
            </template>
            <div :id="syncInfoId" class="grid w-64 gap-2 p-3 text-cp-xs leading-relaxed text-cp-text-secondary">
              <p class="m-0">
                从 models.dev 获取社区价目，保留人工单价与倍率。
              </p>
              <p class="m-0 text-cp-text-tertiary">
                最近同步：{{ syncTime }}
              </p>
            </div>
          </BasePopover>
          <BaseButton :loading="syncing" :disabled="disabled" aria-label="同步价目" @click="$emit('sync')">
            <template #icon>
              <Download class="size-4" />
            </template>
            同步
          </BaseButton>
        </div>
        <BaseButton variant="primary" :disabled="disabled" @click="$emit('add')">
          <template #icon>
            <Plus class="size-4" />
          </template>
          添加模型
        </BaseButton>
      </div>
    </div>
  </div>
</template>
