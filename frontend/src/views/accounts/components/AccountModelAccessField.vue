<script setup lang="ts">
import type { AccountModelAccess } from '@/api'

import { ZCheckbox, ZEmpty, ZFormItem, ZIconButton, ZInput, ZScrollbar, ZSegmented } from '@codex-proxy/ui'

import { RefreshCw, Search } from '@lucide/vue'
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getAccountModels, refreshAccountModels } from '@/api'
import { useRequestState } from '@/composables/useRequestState'
import { accountModelAccessError, accountModelIdError } from '../utils/modelAccess'

const props = withDefaults(defineProps<{
  accountId?: string
  disabled?: boolean
  allowPreserve?: boolean
}>(), { disabled: false, allowPreserve: false })

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<AccountModelAccess | undefined>({ required: true })
const catalog = ref<Array<{ id: string, label: string }>>([])
const search = ref('')
const inputError = ref('')
const request = useRequestState()
const { loading, error } = request
const modes = computed(() => [
  ...(props.allowPreserve
    ? [
        {
          value: 'preserve',
          label: t('accounts.modelAccess.reserve'),
        },
      ]
    : []),
  {
    value: 'all',
    label: t('common.values.unlimited'),
  },
  {
    value: 'allowlist',
    label: t('accounts.modelAccess.allowlist'),
  },
  {
    value: 'denylist',
    label: t('accounts.modelAccess.blocklist'),
  },
])
const mode = computed({
  get: () => model.value?.mode ?? 'preserve',
  set: (value: string) => {
    inputError.value = ''
    if (value === 'preserve') {
      model.value = undefined
      return
    }
    model.value = {
      mode: value as AccountModelAccess['mode'],
      models: value === 'all'
        ? []
        : [
            ...(model.value?.models ?? []),
          ],
    }
  },
})
const restricted = computed(() => mode.value === 'allowlist' || mode.value === 'denylist')
const canAdd = computed(() => Boolean(search.value.trim()) && !model.value?.models.includes(search.value.trim()))
const models = computed(() => {
  const entries = new Map(catalog.value.map(item => [
    item.id,
    { ...item, unavailable: false },
  ]))
  for (const id of model.value?.models ?? []) {
    if (!entries.has(id))
      entries.set(id, { id, label: id, unavailable: true })
  }
  const query = search.value.trim().toLowerCase()
  return [
    ...entries.values(),
  ].filter(item => `${item.id} ${item.label}`.toLowerCase().includes(query))
})

function select(id: string, selected: boolean) {
  if (props.disabled || !model.value || !restricted.value)
    return
  const ids = new Set(model.value.models)
  if (selected)
    ids.add(id)
  else ids.delete(id)
  const next = {
    ...model.value,
    models: [...ids],
  }
  inputError.value = selected
    ? accountModelAccessError(next) ?? ''
    : ''
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

watch([
  () => props.accountId,
  restricted,
], () => {
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
    <ZFormItem :label="t('accounts.modelAccess.modelLimitations')">
      <template v-if="restricted && accountId" #extra>
        <ZIconButton :aria-label="t('accounts.modelAccess.refreshModel')" size="small" :loading="loading" :disabled="disabled" @click="load(true)">
          <template #loading>
            <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-3.5" />
        </ZIconButton>
      </template>
      <ZSegmented v-model="mode" class="w-80 max-w-full" :aria-label="t('accounts.modelAccess.modelRestrictionMode')" :options="modes" :disabled="disabled" />
    </ZFormItem>
    <template v-if="restricted">
      <ZInput v-model="search" :aria-label="t('accounts.modelAccess.searchOrAddModels')" :placeholder="t('accounts.modelAccess.searchOrEnterIdAndPressEnter')" :disabled="disabled" :aria-invalid="Boolean(inputError)" @keydown.enter.prevent="addModel">
        <template #prefix>
          <Search class="size-4" aria-hidden="true" />
        </template>
      </ZInput>
      <p v-if="inputError" class="m-0 text-cp-xs text-cp-error-text" role="alert">
        {{ inputError }}
      </p>
      <ZScrollbar v-if="models.length" max-height="15rem" class="min-w-0 -m-1">
        <div class="grid grid-cols-2 gap-2 p-1 sm:grid-cols-3" role="group" :aria-label="t('accounts.modelAccess.selectModel')" :aria-busy="loading || undefined">
          <ZCheckbox
            v-for="item in models"
            :key="item.id"
            class="min-h-11 min-w-0 rounded-cp px-3 py-2.5 transition-colors duration-150 motion-reduce:transition-none"
            :class="model?.models.includes(item.id) ? 'bg-cp-primary-container text-cp-primary-on-container' : 'bg-cp-fill-quaternary text-cp-text-secondary hover:bg-cp-fill-tertiary hover:text-cp-text'"
            :model-value="model?.models.includes(item.id) ?? false"
            :label="item.id"
            :title="item.unavailable && accountId ? t('accounts.modelAccess.notInCurrentModelCatalog') : item.id"
            :disabled="disabled"
            @update:model-value="select(item.id, $event)"
          >
            <template #label>
              <span class="block truncate" :class="{ 'text-cp-error-text': item.unavailable }">{{ item.id }}</span>
            </template>
          </ZCheckbox>
        </div>
      </ZScrollbar>
      <p v-else-if="loading" class="m-0 py-4 text-center text-cp-sm text-cp-text-tertiary" role="status">
        {{ t('accounts.modelAccess.loadingModel') }}
      </p>
      <ZEmpty v-else-if="!error" :title="search ? t('accounts.modelAccess.noMatchingModels') : t('accounts.modelAccess.noModels')" :icon="search ? Search : undefined" size="small" surface="none" />
      <ZEmpty v-if="error" :title="t('accounts.modelAccess.modelListLoadingFailed')" :description="t('accounts.modelAccess.retryHint')" size="small" surface="none" role="alert" />
    </template>
  </div>
</template>
