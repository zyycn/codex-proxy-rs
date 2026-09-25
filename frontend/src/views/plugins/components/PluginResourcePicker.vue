<script setup lang="ts">
import { BaseButton, BaseEmpty, BaseSelect, toast } from '@codex-proxy/ui'
import { CircleAlert, LoaderCircle } from '@lucide/vue'
import { computed, onScopeDispose, shallowRef, watch } from 'vue'
import { getAccountGroups, getApiKeys } from '@/api'
import { errorMessage } from '@/utils/async'

const props = withDefaults(defineProps<{ kind: 'keys' | 'groups', disabled?: boolean, maxCollapseTags?: number }>(), { maxCollapseTags: 1 })
const selected = defineModel<string[]>({ required: true })
const options = shallowRef<{ id: string, label: string, disabled: boolean }[]>([])
const loading = shallowRef(false)
const error = shallowRef('')
const label = computed(() => props.kind === 'keys' ? '客户端 Key' : '账号分组')
let controller: AbortController | undefined
const choices = computed(() => {
  const known = new Set(options.value.map(option => option.id))
  const unavailable = selected.value.filter(id => !known.has(id)).map(id => ({ id, label: `不可用 · ${id}`, disabled: true }))
  return [...unavailable, ...options.value].map(option => ({
    value: option.id,
    label: option.label,
    description: option.disabled ? '不可用' : undefined,
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
      if (!toast.messages.some(message => message.type === 'error' && message.message === error.value))
        toast.error(error.value)
    }
  }
  finally {
    if (controller === current)
      loading.value = false
  }
}
watch(() => props.kind, load, { immediate: true })
onScopeDispose(() => controller?.abort())
</script>

<template>
  <BaseSelect
    v-model="selected"
    :options="choices"
    multiple
    collapse-tags
    collapse-tags-tooltip
    :max-collapse-tags="maxCollapseTags"
    :filterable="choices.length > 6"
    :loading="loading || Boolean(error)"
    :disabled="disabled"
    :aria-label="`${label}（可多选）`"
    placeholder="不限（可多选）"
    class="w-full min-w-0"
  >
    <template #empty="{ search }">
      <BaseEmpty v-if="loading" title="正在加载…" size="sm" surface="none" role="status">
        <template #icon>
          <LoaderCircle :size="18" class="animate-spin text-cp-text-quaternary motion-reduce:animate-none" />
        </template>
      </BaseEmpty>
      <BaseEmpty v-else-if="error" title="加载失败" :description="`暂时无法获取${label}`" :icon="CircleAlert" size="sm" surface="none" role="alert">
        <template #action>
          <BaseButton size="sm" variant="secondary" :disabled="disabled" @click="load">
            重试
          </BaseButton>
        </template>
      </BaseEmpty>
      <BaseEmpty
        v-else
        :title="search ? '没有匹配项' : `暂无${label}`"
        :description="search ? undefined : `请先在${kind === 'keys' ? 'API 密钥' : '分组管理'}中创建`"
        size="sm"
        surface="none"
        role="status"
      />
    </template>
  </BaseSelect>
</template>
