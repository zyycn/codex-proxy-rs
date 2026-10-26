<script setup lang="ts">
import { ZButton, ZEmpty, ZSelect } from '@codex-proxy/ui'
import { CircleAlert, LoaderCircle } from '@lucide/vue'

import { computed, onScopeDispose, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getAccountGroups, getApiKeys } from '@/api'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage } from '@/utils/operation'

const props = withDefaults(defineProps<{ kind: 'keys' | 'groups', disabled?: boolean, active?: boolean, maxCollapseTags?: number }>(), { active: true, maxCollapseTags: 1 })

const { t } = useI18n({ useScope: 'global' })

const selected = defineModel<string[]>({ required: true })
const options = shallowRef<{ id: string, label: string, disabled: boolean }[]>([])
const loading = shallowRef(false)
const error = shallowRef('')
const label = computed(() => props.kind === 'keys'
  ? t('plugins.bindings.clientKey')
  : t('plugins.bindings.accountGrouping'))
let controller: AbortController | undefined
const choices = computed(() => {
  const known = new Set(options.value.map(option => option.id))
  const unavailable = selected.value.filter(id => !known.has(id)).map(id => ({
    id,
    label: t('plugins.resources.unavailable', { id }),
    disabled: true,
  }))
  return [
    ...unavailable,
    ...options.value,
  ].map(option => ({
    value: option.id,
    label: option.label,
    description: option.disabled
      ? t('common.status.unavailable')
      : undefined,
    // 已失效的绑定仍可取消，不能重新选入。
    disabled: option.disabled && !selected.value.includes(option.id),
  }))
})
async function load() {
  controller?.abort()
  const current = new AbortController()
  controller = current
  loading.value = true
  error.value = ''
  const items: typeof options.value = []
  try {
    const requestOptions = { signal: current.signal, silent: true }
    if (props.kind === 'keys') {
      let cursor: string | undefined
      do {
        const result = await getApiKeys({ limit: 200, cursor }, requestOptions)
        items.push(...result.items.map(item => ({ id: item.id, label: `${item.name} · ${item.prefix}`, disabled: !item.enabled })))
        cursor = result.nextCursor ?? undefined
      } while (cursor)
    }
    else {
      let page = 1
      let totalPages = 1
      do {
        const result = await getAccountGroups({ page, pageSize: 100 }, requestOptions)
        items.push(...result.items.map(item => ({ id: item.id, label: item.name, disabled: !item.enabled })))
        totalPages = result.page.totalPages
        page++
      } while (page <= totalPages)
    }
    if (controller === current)
      options.value = items
  }
  catch (cause) {
    if (!current.signal.aborted) {
      error.value = errorMessage(cause)
      // 同一弹窗可能挂载多个范围选择器，只显示一份相同错误。
      ZNotification.error({ message: error.value, deduplicate: true })
    }
  }
  finally {
    if (controller === current)
      loading.value = false
  }
}
watch([
  () => props.kind,
  () => props.active,
], ([, active]) => {
  controller?.abort()
  controller = undefined
  if (active)
    void load()
}, { immediate: true, flush: 'sync' })
onScopeDispose(() => controller?.abort())
</script>

<template>
  <ZSelect
    v-model="selected"
    :options="choices"
    multiple
    collapse-tags
    collapse-tags-tooltip
    :max-collapse-tags="maxCollapseTags"
    :filterable="choices.length > 6"
    :loading="loading || Boolean(error)"
    :disabled="disabled"
    :aria-label="t('plugins.resources.multipleSelectionsAllowed', { label })"
    :placeholder="t('plugins.resources.unlimitedPlaceholder')"
    class="w-full min-w-0"
  >
    <template #empty="{ search }">
      <ZEmpty v-if="loading" :title="t('plugins.resources.loading')" size="small" surface="none" role="status">
        <template #icon>
          <LoaderCircle :size="18" class="animate-spin text-cp-text-quaternary motion-reduce:animate-none" />
        </template>
      </ZEmpty>
      <ZEmpty v-else-if="error" :title="t('common.errors.load')" :description="t('plugins.resources.isTemporarilyUnavailable', { label })" :icon="CircleAlert" size="small" surface="none" role="alert">
        <template #action>
          <ZButton size="small" :disabled="disabled" @click="load">
            {{ t('common.actions.retry') }}
          </ZButton>
        </template>
      </ZEmpty>
      <ZEmpty
        v-else
        :title="search ? t('plugins.resources.noMatches') : t('plugins.resources.no', { label })"
        :description="search ? undefined : t('plugins.resources.createOneInFirst', { value: kind === 'keys' ? t('navigation.apiKeys') : t('navigation.groups') })"
        size="small"
        surface="none"
        role="status"
      />
    </template>
  </ZSelect>
</template>
