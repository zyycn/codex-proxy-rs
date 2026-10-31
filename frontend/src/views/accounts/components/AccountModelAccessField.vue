<script setup lang="ts">
import type { AccountModelAccess } from '@/api'

import { ZCheckbox, ZEmpty, ZFormItem, ZIconButton, ZInput, ZPopover, ZScrollbar, ZSegmented } from '@codex-proxy/ui'

import { CircleAlert, RefreshCw, Search, TriangleAlert } from '@lucide/vue'
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
// 未取得目录时不能判断缺失，成功返回空目录则可以
const catalog = ref<Array<{ id: string, label: string }>>()
const catalogObservedAtDisplay = ref<string>()
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
  const catalogReady = catalog.value !== undefined && !loading.value && !error.value
  const entries = new Map((catalog.value ?? []).map(item => [
    item.id,
    { ...item, unavailable: false },
  ]))
  for (const id of model.value?.models ?? []) {
    if (!entries.has(id))
      entries.set(id, { id, label: id, unavailable: catalogReady })
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

function handleCatalogHelpKeydown(event: KeyboardEvent, open: boolean, close: () => void) {
  if (event.key === 'Escape' && open) {
    // 先关闭说明浮层，避免同一次按键也关闭编辑弹窗
    event.preventDefault()
    event.stopPropagation()
    close()
  }
}

async function load(refresh = false) {
  const accountId = props.accountId
  if (!accountId || !restricted.value)
    return
  const requestId = request.start()
  try {
    const result = await (refresh ? refreshAccountModels : getAccountModels)(
      { accountId },
      { signal: request.signal },
    )
    if (request.isCurrent(requestId)) {
      catalog.value = result.models
      catalogObservedAtDisplay.value = result.observedAt ? result.observedAtDisplay : undefined
    }
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
  catalog.value = undefined
  catalogObservedAtDisplay.value = undefined
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
    <ZFormItem>
      <template #label>
        <span class="inline-flex flex-wrap items-end gap-x-2 gap-y-1">
          <span>{{ t('accounts.modelAccess.modelCatalog') }}</span>
          <span v-if="restricted && catalogObservedAtDisplay" class="text-cp-xs leading-none font-normal text-cp-text-tertiary tabular-nums">
            {{ catalogObservedAtDisplay }}
          </span>
        </span>
      </template>
      <template v-if="restricted" #label-extra>
        <ZPopover class="-my-1 -ml-1.5 shrink-0" trigger="hover-click" placement="top-start" role="tooltip">
          <template #reference="{ open, id, close }">
            <ZIconButton
              variant="link"
              class="size-6!"
              :aria-label="t('accounts.modelAccess.catalogHelpTitle')"
              :aria-describedby="open ? id : undefined"
              :aria-expanded="open"
              :title="undefined"
              @keydown="handleCatalogHelpKeydown($event, open, close)"
            >
              <CircleAlert class="size-[1em] text-cp-xs" aria-hidden="true" />
            </ZIconButton>
          </template>
          <div class="max-w-72 space-y-2 px-3 py-2 text-cp-sm leading-relaxed text-cp-text-secondary">
            <p class="m-0">
              {{ t('accounts.modelAccess.catalogObservedAtHelp') }}
            </p>
            <p class="m-0">
              {{ t('accounts.modelAccess.catalogDescription') }}
            </p>
          </div>
        </ZPopover>
      </template>
      <div class="flex min-w-0 items-center justify-between gap-2">
        <ZSegmented v-model="mode" class="w-80 min-w-0 max-w-full" :aria-label="t('accounts.modelAccess.modelRestrictionMode')" :options="modes" :disabled="disabled" />
        <ZIconButton v-if="restricted && accountId" :aria-label="t('accounts.modelAccess.refreshModel')" variant="plain" :loading="loading" :disabled="disabled" @click="load(true)">
          <template #loading>
            <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
          </template>
          <RefreshCw class="size-3.5" />
        </ZIconButton>
      </div>
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
          <div
            v-for="item in models"
            :key="item.id"
            class="flex min-w-0 items-center rounded-cp transition-colors duration-150 motion-reduce:transition-none"
            :class="model?.models.includes(item.id) ? 'bg-cp-primary-container text-cp-primary-on-container' : 'bg-cp-fill-quaternary text-cp-text-secondary hover:bg-cp-fill-tertiary hover:text-cp-text'"
          >
            <ZCheckbox
              class="min-h-11 min-w-0 flex-1 px-3 py-2.5"
              :model-value="model?.models.includes(item.id) ?? false"
              :label="item.id"
              :title="item.id"
              :disabled="disabled"
              @update:model-value="select(item.id, $event)"
            >
              <template #label>
                <span class="block truncate">{{ item.id }}</span>
              </template>
            </ZCheckbox>
            <ZPopover v-if="item.unavailable" class="mr-1 shrink-0" trigger="hover-click" placement="top" role="tooltip">
              <template #reference="{ open, id }">
                <ZIconButton
                  variant="link"
                  size="small"
                  :aria-label="t('accounts.modelAccess.notInCurrentModelCatalog', { id: item.id })"
                  :aria-describedby="open ? id : undefined"
                  :aria-expanded="open"
                  :title="undefined"
                >
                  <TriangleAlert class="size-3.5 text-cp-warning-text" aria-hidden="true" />
                </ZIconButton>
              </template>
              <div class="max-w-64 space-y-1 px-3 py-2 text-cp-sm wrap-anywhere">
                <p class="m-0 font-emphasis text-cp-text">
                  {{ item.id }}
                </p>
                <p class="m-0 text-cp-text-secondary">
                  {{ t('accounts.modelAccess.modelCatalogMissingHint') }}
                </p>
              </div>
            </ZPopover>
          </div>
        </div>
      </ZScrollbar>
      <p v-else-if="loading" class="m-0 py-4 text-center text-cp-sm text-cp-text-tertiary" role="status">
        {{ t('accounts.modelAccess.loadingModel') }}
      </p>
      <ZEmpty v-else-if="!error" :title="search ? t('accounts.modelAccess.noMatchingModels') : t('accounts.modelAccess.noModels')" :icon="search ? Search : undefined" size="small" surface="none" />
    </template>
  </div>
</template>
