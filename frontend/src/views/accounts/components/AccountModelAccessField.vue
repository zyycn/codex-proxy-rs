<script setup lang="ts">
import type { AccountModelAccess } from '@/api'

import { RefreshCw, Search } from '@lucide/vue'
import { computed, ref, watch } from 'vue'
import { getAccountModels, refreshAccountModels } from '@/api'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import { useRequestState } from '@/composables/useRequestState'
import { accountModelAccessError, accountModelIdError } from '../utils/modelAccess'

const props = withDefaults(defineProps<{
  accountId?: string
  disabled?: boolean
  allowPreserve?: boolean
}>(), { disabled: false, allowPreserve: false })
const model = defineModel<AccountModelAccess | undefined>({ required: true })
const catalog = ref<Array<{ id: string, label: string }>>([])
const search = ref('')
const inputError = ref('')
const request = useRequestState()
const { loading, error } = request
const modes = computed(() => [
  ...(props.allowPreserve ? [{ value: 'preserve', label: '保留' }] : []),
  { value: 'all', label: '不限制' },
  { value: 'allowlist', label: '白名单' },
  { value: 'denylist', label: '黑名单' },
])
const mode = computed({
  get: () => model.value?.mode ?? 'preserve',
  set: (value: string) => {
    inputError.value = ''
    if (value === 'preserve') {
      model.value = undefined
      return
    }
    model.value = { mode: value as AccountModelAccess['mode'], models: value === 'all' ? [] : [...(model.value?.models ?? [])] }
  },
})
const restricted = computed(() => mode.value === 'allowlist' || mode.value === 'denylist')
const canAdd = computed(() => Boolean(search.value.trim()) && !model.value?.models.includes(search.value.trim()))
const models = computed(() => {
  const entries = new Map(catalog.value.map(item => [item.id, { ...item, unavailable: false }]))
  for (const id of model.value?.models ?? []) {
    if (!entries.has(id))
      entries.set(id, { id, label: id, unavailable: true })
  }
  const query = search.value.trim().toLowerCase()
  return [...entries.values()].filter(item => `${item.id} ${item.label}`.toLowerCase().includes(query))
})

function select(id: string, selected: boolean) {
  if (props.disabled || !model.value || !restricted.value)
    return
  const ids = new Set(model.value.models)
  if (selected)
    ids.add(id)
  else ids.delete(id)
  const next = { ...model.value, models: [...ids] }
  inputError.value = selected ? accountModelAccessError(next) ?? '' : ''
  if (!inputError.value)
    model.value = next
}

function addModel() {
  if (props.disabled || !restricted.value || !canAdd.value)
    return
  const id = search.value.trim()
  inputError.value = accountModelIdError(id) ?? ''
  if (inputError.value)
    return
  select(id, true)
  if (!inputError.value)
    search.value = ''
}

async function load(refresh = false) {
  const accountId = props.accountId
  if (!accountId || !restricted.value)
    return
  const requestId = request.start()
  try {
    const result = await (refresh ? refreshAccountModels : getAccountModels)(
      { accountId },
      { signal: request.signal, silent: true },
    )
    if (request.isCurrent(requestId))
      catalog.value = result.models
  }
  catch (cause) {
    request.fail(requestId, cause)
  }
  finally {
    request.finish(requestId)
  }
}

watch([() => props.accountId, restricted], () => {
  request.invalidate()
  catalog.value = []
  search.value = ''
  inputError.value = ''
  error.value = ''
  void load()
}, { immediate: true })

watch(search, () => {
  inputError.value = ''
})
</script>

<template>
  <div class="grid gap-3">
    <BaseFormItem label="模型限制">
      <template v-if="restricted && accountId" #extra>
        <BaseIconButton label="刷新模型" size="sm" :loading="loading" :disabled="disabled" @click="load(true)">
          <template #loading>
            <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-3.5" />
        </BaseIconButton>
      </template>
      <BaseSegmented v-model="mode" class="w-80 max-w-full" label="模型限制模式" :options="modes" :disabled="disabled" />
    </BaseFormItem>
    <template v-if="restricted">
      <BaseInput v-model="search" aria-label="搜索或添加模型" placeholder="搜索或输入 ID，回车添加" :disabled="disabled" :aria-invalid="Boolean(inputError)" @keydown.enter.prevent="addModel">
        <template #prefix>
          <Search class="size-4" aria-hidden="true" />
        </template>
      </BaseInput>
      <p v-if="inputError" class="m-0 text-cp-xs text-cp-error-text" role="alert">
        {{ inputError }}
      </p>
      <BaseScrollbar v-if="models.length" max-height="15rem" class="min-w-0 -m-1">
        <div class="grid grid-cols-2 gap-2 p-1 sm:grid-cols-3" role="group" aria-label="选择模型" :aria-busy="loading || undefined">
          <BaseCheckbox
            v-for="item in models"
            :key="item.id"
            class="min-h-11 min-w-0 rounded-cp px-3 py-2.5 transition-colors duration-150 motion-reduce:transition-none"
            :class="model?.models.includes(item.id) ? 'bg-cp-primary-container text-cp-primary-on-container' : 'bg-cp-fill-quaternary text-cp-text-secondary hover:bg-cp-fill-tertiary hover:text-cp-text'"
            :model-value="model?.models.includes(item.id) ?? false"
            :label="item.id"
            :title="item.unavailable && accountId ? `${item.id}（当前目录未返回）` : item.id"
            show-label
            :disabled="disabled"
            @update:model-value="select(item.id, $event)"
          >
            <template #label>
              <span class="block truncate">{{ item.id }}</span>
            </template>
          </BaseCheckbox>
        </div>
      </BaseScrollbar>
      <p v-else-if="loading" class="m-0 py-4 text-center text-cp-sm text-cp-text-tertiary" role="status">
        加载模型中…
      </p>
      <BaseEmpty v-else-if="!error" :title="search ? '无匹配模型' : '暂无模型'" :icon="search ? Search : undefined" size="sm" surface="none" />
      <BaseEmpty v-if="error" title="模型列表加载失败" description="请刷新后重试" size="sm" surface="none" role="alert" />
    </template>
  </div>
</template>
