<script setup lang="ts">
import type { AccountModelAccess } from '@/api'

import { ZButton, ZEmpty, ZFormItem, ZIconButton, ZInput, ZPopover, ZScrollbar, ZSegmented } from '@codex-proxy/ui'

import { CircleAlert, ListChecks, ListMinus, Plus, RefreshCw, Search } from '@lucide/vue'
import { computed, ref, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getAccountModels, refreshAccountModels } from '@/api'
import { useRequestState } from '@/composables/useRequestState'
import { accountModelAccessError, accountModelIdError } from '../utils/modelAccess'
import AccountModelCleanupPopover from './AccountModelCleanupPopover.vue'
import AccountModelOption from './AccountModelOption.vue'

const props = withDefaults(defineProps<{
  accountId?: string
  disabled?: boolean
  allowPreserve?: boolean
  formProp: string
}>(), { disabled: false, allowPreserve: false })

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<AccountModelAccess | undefined>({ required: true })
const formItem = useTemplateRef<InstanceType<typeof ZFormItem>>('modelAccessItem')
const refreshButton = useTemplateRef<InstanceType<typeof ZIconButton>>('refreshButton')
watch(model, () => formItem.value?.clearValidate())
// 未取得目录时不能判断缺失，成功返回空目录则可以
const catalog = ref<Array<{ id: string, label: string }>>()
const catalogObservedAtDisplay = ref<string>()
const search = ref('')
const searchInput = useTemplateRef<InstanceType<typeof ZInput>>('searchInput')
const selectionError = ref('')
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
    selectionError.value = ''
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
const selectedIds = computed(() => new Set(model.value?.models))
const catalogReady = computed(() => catalog.value !== undefined && !loading.value && !error.value)
const extraIds = computed(() => {
  const ids = new Set(catalog.value?.map(item => item.id))
  return (model.value?.models ?? []).filter(id => !ids.has(id))
})
// 目录已就绪且非空时才能按目录清理白名单
const missingModels = computed(() => mode.value === 'allowlist' && catalogReady.value && catalog.value?.length
  ? extraIds.value
  : [])
const query = computed(() => search.value.trim().toLowerCase())
const catalogResults = computed(() => (catalog.value ?? [])
  .filter(item => `${item.id} ${item.label}`.toLowerCase().includes(query.value))
  .map(item => item.id))
const allResultsSelected = computed(() => catalogResults.value.every(id => selectedIds.value.has(id)))
const groups = computed(() => [
  {
    key: 'selected',
    label: catalogReady.value
      ? t('accounts.modelAccess.customModels')
      : t('accounts.modelAccess.selectedModels'),
    ids: extraIds.value.filter(id => id.toLowerCase().includes(query.value)),
  },
  {
    key: 'catalog',
    label: t('accounts.modelAccess.upstreamCatalog'),
    ids: catalogResults.value,
  },
].filter(group => group.ids.length))
const newModelId = computed(() => groups.value.length
  ? ''
  : search.value.trim())
const inputError = computed(() => selectionError.value || (newModelId.value && accountModelIdError(newModelId.value)))
const canAdd = computed(() => Boolean(newModelId.value) && !inputError.value)

function removeMissingModels(ids: string[]) {
  if (props.disabled || model.value?.mode !== 'allowlist')
    return
  const selected = new Set(ids)
  const removed = new Set(missingModels.value.filter(id => selected.has(id)))
  if (!removed.size)
    return
  const next = {
    ...model.value,
    models: model.value.models.filter(id => !removed.has(id)),
  }
  selectionError.value = accountModelAccessError(next) ?? ''
  if (!selectionError.value)
    model.value = next
}

function restoreCleanupFocus() {
  // 最后一项清理后入口禁用，改回相邻刷新按钮，避免焦点落到页面背景
  if (!missingModels.value.length && document.activeElement === document.body)
    refreshButton.value?.focus()
}

function select(modelIds: string[], selected: boolean) {
  if (props.disabled || !model.value || !restricted.value)
    return
  const ids = new Set(selectedIds.value)
  for (const id of modelIds) {
    if (selected)
      ids.add(id)
    else ids.delete(id)
  }
  const next = {
    ...model.value,
    models: [...ids],
  }
  selectionError.value = selected
    ? accountModelAccessError(next) ?? ''
    : ''
  if (!selectionError.value)
    model.value = next
}

function addModel() {
  if (props.disabled || !restricted.value || !canAdd.value)
    return
  select([newModelId.value], true)
  if (!selectionError.value) {
    search.value = ''
    searchInput.value?.focus()
  }
}

function handleSearchEnter(event: KeyboardEvent) {
  if (event.isComposing)
    return
  event.preventDefault()
  addModel()
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
  selectionError.value = ''
  error.value = ''
  void load()
}, { immediate: true })

watch(search, () => {
  selectionError.value = ''
})
</script>

<template>
  <div class="@container grid gap-4.5">
    <ZFormItem ref="modelAccessItem" :prop="formProp">
      <template #label>
        <span class="inline-flex flex-wrap items-end gap-x-2 gap-y-1">
          <span>{{ t('accounts.modelAccess.modelRestrictionMode') }}</span>
          <span v-if="restricted && catalogObservedAtDisplay" class="text-z-xs leading-none font-normal text-z-text-tertiary tabular-nums">
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
              <CircleAlert class="size-[1em] text-z-xs" aria-hidden="true" />
            </ZIconButton>
          </template>
          <div class="max-w-72 space-y-2 px-3 py-2 text-z-sm leading-relaxed text-z-text-secondary">
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
        <div v-if="restricted && accountId" class="flex shrink-0 items-center gap-1">
          <AccountModelCleanupPopover v-if="mode === 'allowlist'" :models="missingModels" :model-count="model?.models.length ?? 0" :disabled="disabled || !missingModels.length" @remove="removeMissingModels" @closed="restoreCleanupFocus" />
          <ZIconButton ref="refreshButton" :aria-label="t('accounts.modelAccess.refreshModel')" variant="plain" :loading="loading" :disabled="disabled" @click="load(true)">
            <template #loading>
              <RefreshCw class="size-3.5 animate-spin motion-reduce:animate-none" />
            </template>
            <RefreshCw class="size-3.5" />
          </ZIconButton>
        </div>
      </div>
    </ZFormItem>
    <template v-if="restricted">
      <div class="flex min-w-0 items-center gap-2">
        <ZInput ref="searchInput" v-model="search" class="min-w-0 flex-1" :aria-label="t('accounts.modelAccess.searchOrAddModels')" :placeholder="t('accounts.modelAccess.searchOrEnterId')" :disabled="disabled" :aria-invalid="Boolean(inputError)" @keydown.enter="handleSearchEnter">
          <template #prefix>
            <Search class="size-4" aria-hidden="true" />
          </template>
        </ZInput>
        <ZButton class="shrink-0" :disabled="disabled || !canAdd" @click="addModel">
          <Plus class="size-4" aria-hidden="true" />
          {{ t('accounts.modelAccess.addModel') }}
        </ZButton>
      </div>
      <p v-if="inputError" class="m-0 text-z-xs text-z-error-text" role="alert">
        {{ inputError }}
      </p>
      <ZScrollbar v-if="groups.length" max-height="15rem" class="min-w-0 -m-1">
        <div class="grid gap-3 p-1" :aria-busy="loading || undefined">
          <div v-for="group in groups" :key="group.key" role="group" :aria-label="group.label">
            <div class="mb-1.5 flex items-center justify-between gap-2">
              <span class="text-z-xs font-medium text-z-text-secondary">{{ group.label }}</span>
              <ZIconButton
                v-if="group.key === 'catalog'"
                type="primary"
                variant="link"
                size="small"
                class="-my-1 size-6!"
                :aria-label="allResultsSelected ? t('accounts.modelAccess.deselectAll') : t('accounts.modelAccess.selectAll')"
                :disabled="disabled || loading"
                @click="select(group.ids, !allResultsSelected)"
              >
                <ListMinus v-if="allResultsSelected" aria-hidden="true" />
                <ListChecks v-else aria-hidden="true" />
              </ZIconButton>
            </div>
            <div class="grid grid-cols-2 gap-2 @xl:grid-cols-3 @3xl:grid-cols-4">
              <div
                v-for="id in group.ids"
                :key="id"
                class="flex min-w-0 items-center rounded-z transition-colors duration-150 motion-reduce:transition-none"
                :class="selectedIds.has(id) ? 'bg-z-primary-container text-z-primary-on-container' : 'bg-z-fill-quaternary text-z-text-secondary hover:bg-z-fill-tertiary hover:text-z-text'"
              >
                <AccountModelOption :model-value="selectedIds.has(id)" :label="id" :disabled="disabled" @update:model-value="select([id], $event)" />
              </div>
            </div>
          </div>
        </div>
      </ZScrollbar>
      <p v-else-if="loading" class="m-0 py-4 text-center text-z-sm text-z-text-tertiary" role="status">
        {{ t('accounts.modelAccess.loadingModel') }}
      </p>
      <p v-else-if="mode === 'denylist' && canAdd" class="m-0 text-z-xs text-z-text-secondary">
        {{ t('accounts.modelAccess.addToDenylistHint') }}
      </p>
      <ZEmpty v-else-if="!error && !search" :title="t('accounts.modelAccess.noModels')" size="small" surface="none" />
    </template>
  </div>
</template>
